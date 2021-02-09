use crate::{
    bitcoin::{
        timelocks::BlockHeight, Address, Amount, BroadcastSignedTransaction, BuildTxLockPsbt,
        GetBlockHeight, GetNetwork, GetRawTransaction, SignTxLock, Transaction,
        TransactionBlockHeight, TxLock, WaitForTransactionFinality, WatchForRawTransaction,
    },
    execution_params::ExecutionParams,
};
use ::bitcoin::{util::psbt::PartiallySignedTransaction, Txid};
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use backoff::{backoff::Constant as ConstantBackoff, tokio::retry};
use bdk::{
    blockchain::{noop_progress, Blockchain, ElectrumBlockchain},
    electrum_client::{Client, ElectrumApi},
    keys::GeneratableDefaultOptions,
    FeeRate,
};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::interval};

const SLED_TREE_NAME: &str = "default_tree";

pub struct Wallet {
    pub inner: Arc<Mutex<bdk::Wallet<ElectrumBlockchain, bdk::sled::Tree>>>,
    pub network: bitcoin::Network,
    pub http_url: Url,
    pub rpc_url: Url,
}

impl Wallet {
    pub async fn new(
        electrum_rpc_url: Url,
        electrum_http_url: Url,
        network: bitcoin::Network,
        datadir: &Path,
    ) -> Result<Self> {
        // todo: Implement conversion to anyhow::error so we can use ?
        let client =
            Client::new(electrum_rpc_url.as_str()).expect("Failed to init electrum rpc client");

        let db = bdk::sled::open(datadir)?.open_tree(SLED_TREE_NAME)?;

        // todo: make key generation configurable using a descriptor
        let p_key = ::bitcoin::PrivateKey::generate_default()?;
        let bdk_wallet = bdk::Wallet::new(
            bdk::template::P2WPKH(p_key),
            None,
            network,
            db,
            ElectrumBlockchain::from(client),
        )?;

        Ok(Self {
            inner: Arc::new(Mutex::new(bdk_wallet)),
            network,
            http_url: electrum_http_url,
            rpc_url: electrum_rpc_url,
        })
    }

    pub async fn balance(&self) -> Result<Amount> {
        self.sync_wallet().await?;
        let balance = self.inner.lock().await.get_balance()?;
        Ok(Amount::from_sat(balance))
    }

    pub async fn new_address(&self) -> Result<Address> {
        self.inner
            .lock()
            .await
            .get_new_address()
            .map_err(Into::into)
    }

    pub async fn get_tx(&self, txid: Txid) -> Result<Option<Transaction>> {
        let tx = self.inner.lock().await.client().get_tx(&txid)?;
        Ok(tx)
    }

    pub async fn transaction_fee(&self, txid: Txid) -> Result<Amount> {
        self.sync_wallet().await?;
        let fees = self
            .inner
            .lock()
            .await
            .list_transactions(true)?
            .iter()
            .find(|tx| tx.txid == txid)
            .ok_or_else(|| {
                anyhow!("Could not find tx in bdk wallet when trying to determine fees")
            })?
            .fees;

        Ok(Amount::from_sat(fees))
    }

    pub async fn sync_wallet(&self) -> Result<()> {
        tracing::debug!("syncing wallet");
        self.inner.lock().await.sync(noop_progress(), None)?;
        Ok(())
    }
}

#[async_trait]
impl BuildTxLockPsbt for Wallet {
    async fn build_tx_lock_psbt(
        &self,
        output_address: Address,
        output_amount: Amount,
    ) -> Result<PartiallySignedTransaction> {
        self.sync_wallet().await?;
        tracing::debug!("building tx lock");
        let (psbt, _details) = self.inner.lock().await.create_tx(
            bdk::TxBuilder::with_recipients(vec![(
                output_address.script_pubkey(),
                output_amount.as_sat(),
            )])
            // todo: get actual fee
            .fee_rate(FeeRate::from_sat_per_vb(5.0)),
        )?;
        tracing::debug!("tx lock built");
        Ok(psbt)
    }
}

#[async_trait]
impl SignTxLock for Wallet {
    async fn sign_tx_lock(&self, tx_lock: TxLock) -> Result<Transaction> {
        self.sync_wallet().await?;
        tracing::debug!("signing tx lock");
        let psbt = PartiallySignedTransaction::from(tx_lock);
        let (signed_psbt, finalized) = self.inner.lock().await.sign(psbt, None)?;
        if !finalized {
            bail!("Could not finalize TxLock psbt")
        }
        let tx = signed_psbt.extract_tx();
        tracing::debug!("signed tx lock");
        Ok(tx)
    }
}

#[async_trait]
impl BroadcastSignedTransaction for Wallet {
    async fn broadcast_signed_transaction(&self, transaction: Transaction) -> Result<Txid> {
        tracing::debug!("attempting to broadcast tx: {}", transaction.txid());
        self.inner.lock().await.broadcast(transaction.clone())?;
        tracing::info!("Bitcoin tx broadcasted! TXID = {}", transaction.txid());
        Ok(transaction.txid())
    }
}

#[async_trait]
impl WatchForRawTransaction for Wallet {
    async fn watch_for_raw_transaction(&self, txid: Txid) -> Transaction {
        tracing::debug!("watching for tx: {}", txid);
        retry(ConstantBackoff::new(Duration::from_secs(1)), || async {
            let client = Client::new(self.rpc_url.as_ref())?;
            let tx = client.transaction_get(&txid)?;
            tracing::debug!("found tx: {}", txid);
            Ok(tx)
        })
        .await
        .expect("transient errors to be retried")
    }
}

#[async_trait]
impl GetRawTransaction for Wallet {
    async fn get_raw_transaction(&self, txid: Txid) -> Result<Transaction> {
        self.get_tx(txid)
            .await?
            .ok_or_else(|| anyhow!("Could not get raw tx with id: {}", txid))
    }
}

#[async_trait]
impl GetBlockHeight for Wallet {
    async fn get_block_height(&self) -> BlockHeight {
        // todo: create this url using the join() api in the Url type
        let url = format!("{}{}", self.http_url.as_str(), "blocks/tip/height");
        #[derive(Debug)]
        enum Error {
            Io(reqwest::Error),
            Parse(std::num::ParseIntError),
        }
        let height = retry(ConstantBackoff::new(Duration::from_secs(1)), || async {
            // todo: We may want to return early if we cannot connect to the electrum node
            // rather than retrying
            let height = reqwest::Client::new()
                .request(Method::GET, &url)
                .send()
                .await
                .map_err(Error::Io)?
                .text()
                .await
                .map_err(Error::Io)?
                .parse::<u32>()
                .map_err(Error::Parse)?;
            Result::<_, backoff::Error<Error>>::Ok(height)
        })
        .await
        .expect("transient errors to be retried");

        BlockHeight::new(height)
    }
}

#[async_trait]
impl TransactionBlockHeight for Wallet {
    async fn transaction_block_height(&self, txid: Txid) -> BlockHeight {
        // todo: create this url using the join() api in the Url type
        let url = format!("{}tx/{}/status", self.http_url, txid);
        #[derive(Serialize, Deserialize, Debug, Clone)]
        struct TransactionStatus {
            block_height: Option<u32>,
            confirmed: bool,
        }
        // todo: See if we can make this error handling more elegant
        // errors
        #[derive(Debug)]
        enum Error {
            Io(reqwest::Error),
            NotYetMined,
            JsonDeserialisation(reqwest::Error),
        }
        let height = retry(ConstantBackoff::new(Duration::from_secs(1)), || async {
            let resp = reqwest::Client::new()
                .request(Method::GET, &url)
                .send()
                .await
                .map_err(|err| backoff::Error::Transient(Error::Io(err)))?;

            let tx_status: TransactionStatus = resp
                .json()
                .await
                .map_err(|err| backoff::Error::Permanent(Error::JsonDeserialisation(err)))?;

            let block_height = tx_status
                .block_height
                .ok_or(backoff::Error::Transient(Error::NotYetMined))?;

            Result::<_, backoff::Error<Error>>::Ok(block_height)
        })
        .await
        .expect("transient errors to be retried");

        BlockHeight::new(height)
    }
}

#[async_trait]
impl WaitForTransactionFinality for Wallet {
    async fn wait_for_transaction_finality(
        &self,
        txid: Txid,
        execution_params: ExecutionParams,
    ) -> Result<()> {
        tracing::debug!("waiting for tx finality: {}", txid);
        // Divide by 4 to not check too often yet still be aware of the new block early
        // on.
        let mut interval = interval(execution_params.bitcoin_avg_block_time / 4);

        loop {
            tracing::debug!("syncing wallet");
            let tx_block_height = self.transaction_block_height(txid).await;
            let block_height = self.get_block_height().await;
            let confirmations = block_height - tx_block_height;
            tracing::debug!("confirmations: {:?}", confirmations);
            if confirmations >= BlockHeight::new(execution_params.bitcoin_finality_confirmations) {
                break;
            }
            interval.tick().await;
        }

        Ok(())
    }
}

#[async_trait]
impl GetNetwork for Wallet {
    async fn get_network(&self) -> bitcoin::Network {
        self.inner.lock().await.network()
    }
}
