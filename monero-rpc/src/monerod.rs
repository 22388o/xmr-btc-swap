use anyhow::{Context, Result};
use curve25519_dalek::edwards::EdwardsPoint;
use monero::{cryptonote::hash::Hash, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_with::{serde_as, TryFromInto};

#[jsonrpc_client::api(version = "2.0")]
pub trait MonerodRpc {
    async fn generateblocks(&self, amount_of_blocks: u32, wallet_address: String)
        -> GenerateBlocks;
    async fn get_block_header_by_height(&self, height: u32) -> BlockHeader;
    async fn get_block_count(&self) -> BlockCount;
    async fn get_block(&self, height: u32) -> GetBlockResponse;
}

#[jsonrpc_client::implement(MonerodRpc)]
#[derive(Debug, Clone)]
pub struct Client {
    inner: reqwest::Client,
    base_url: reqwest::Url,
    get_o_indexes_bin_url: reqwest::Url,
    get_outs_bin_url: reqwest::Url,
    get_transactions: reqwest::Url,
    send_raw_transaction: reqwest::Url,
}

impl Client {
    /// New local host monerod RPC client.
    pub fn localhost(port: u16) -> Result<Self> {
        Self::new("127.0.0.1".to_owned(), port)
    }

    fn new(host: String, port: u16) -> Result<Self> {
        Ok(Self {
            inner: reqwest::ClientBuilder::new()
                .connection_verbose(true)
                .build()?,
            base_url: format!("http://{}:{}/json_rpc", host, port)
                .parse()
                .context("url is well formed")?,
            get_o_indexes_bin_url: format!("http://{}:{}/get_o_indexes.bin", host, port)
                .parse()
                .context("url is well formed")?,
            get_outs_bin_url: format!("http://{}:{}/get_outs.bin", host, port)
                .parse()
                .context("url is well formed")?,
            get_transactions: format!("http://{}:{}/get_transactions", host, port)
                .parse()
                .context("url is well formed")?,
            send_raw_transaction: format!("http://{}:{}/send_raw_transaction", host, port)
                .parse()
                .context("url is well formed")?,
        })
    }

    pub async fn get_transactions(&self, txids: &[Hash]) -> Result<Vec<Transaction>> {
        let response = self
            .inner
            .post(self.get_transactions.clone())
            .json(&GetTransactionsPayload {
                txs_hashes: txids.iter().map(|id| format!("{:x}", id)).collect(),
            })
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Request failed with status code {}", response.status())
        }

        let response = response.json::<GetTransactionsResponse>().await?;

        Ok(response.txs.into_iter().map(|e| e.as_hex).collect())
    }

    pub async fn get_o_indexes(&self, txid: Hash) -> Result<GetOIndexesResponse> {
        self.binary_request(self.get_o_indexes_bin_url.clone(), GetOIndexesPayload {
            txid,
        })
        .await
    }

    pub async fn get_outs(&self, outputs: Vec<GetOutputsOut>) -> Result<GetOutsResponse> {
        self.binary_request(self.get_outs_bin_url.clone(), GetOutsPayload { outputs })
            .await
    }

    pub async fn send_raw_transaction(&self, tx: Transaction) -> Result<()> {
        let tx_as_hex = hex::encode(monero::consensus::encode::serialize(&tx));

        let response = self
            .inner
            .post(self.send_raw_transaction.clone())
            .json(&SendRawTransactionRequest { tx_as_hex })
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Request failed with status code {}", response.status())
        }

        let response = response.json::<SendRawTransactionResponse>().await?;

        if response.status == Status::Failed {
            anyhow::bail!("Response status failed: {:?}", response)
        }

        Ok(())
    }

    async fn binary_request<Req, Res>(&self, url: reqwest::Url, request: Req) -> Result<Res>
    where
        Req: Serialize,
        Res: DeserializeOwned,
    {
        let response = self
            .inner
            .post(url)
            .body(monero_epee_bin_serde::to_bytes(&request)?)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Request failed with status code {}", response.status())
        }

        let body = response.bytes().await?;

        Ok(monero_epee_bin_serde::from_bytes(body)?)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct GenerateBlocks {
    pub blocks: Vec<String>,
    pub height: u32,
}

#[derive(Copy, Clone, Debug, Deserialize)]
pub struct BlockCount {
    pub count: u32,
}

// We should be able to use monero-rs for this but it does not include all
// the fields.
#[derive(Clone, Debug, Deserialize)]
pub struct BlockHeader {
    pub block_size: u32,
    pub depth: u32,
    pub difficulty: u32,
    pub hash: String,
    pub height: u32,
    pub major_version: u32,
    pub minor_version: u32,
    pub nonce: u32,
    pub num_txes: u32,
    pub orphan_status: bool,
    pub prev_hash: String,
    pub reward: u64,
    pub timestamp: u32,
}

#[derive(Debug, Deserialize)]
pub struct GetBlockResponse {
    #[serde(with = "monero_serde_hex_block")]
    pub blob: monero::Block,
}

#[derive(Debug, Deserialize)]
pub struct GetIndexesResponse {
    pub o_indexes: Vec<u32>,
}

#[derive(Clone, Debug, Serialize)]
struct GetTransactionsPayload {
    txs_hashes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct GetTransactionsResponse {
    txs: Vec<GetTransactionsResponseEntry>,
}

#[derive(Clone, Debug, Deserialize)]
struct GetTransactionsResponseEntry {
    #[serde(with = "monero_serde_hex_transaction")]
    as_hex: Transaction,
}

#[serde_as]
#[derive(Clone, Debug, Serialize)]
struct GetOIndexesPayload {
    #[serde_as(as = "TryFromInto<[u8; 32]>")]
    txid: Hash,
}

#[derive(Clone, Debug, Serialize)]
struct GetOutsPayload {
    outputs: Vec<GetOutputsOut>,
}

#[derive(Copy, Clone, Debug, Serialize, PartialEq)]
pub struct GetOutputsOut {
    pub amount: u64,
    pub index: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct GetOutsResponse {
    #[serde(flatten)]
    pub base: BaseResponse,
    pub outs: Vec<OutKey>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SendRawTransactionRequest {
    pub tx_as_hex: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SendRawTransactionResponse {
    pub status: Status,
    pub reason: String,
    pub double_spend: bool,
    pub fee_too_low: bool,
    pub invalid_input: bool,
    pub invalid_output: bool,
    pub low_mixin: bool,
    pub not_relayed: bool,
    pub overspend: bool,
    pub too_big: bool,
}

#[serde_as]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct OutKey {
    pub height: u64,
    #[serde_as(as = "TryFromInto<[u8; 32]>")]
    pub key: EdwardsPoint,
    #[serde_as(as = "TryFromInto<[u8; 32]>")]
    pub mask: EdwardsPoint,
    #[serde_as(as = "TryFromInto<[u8; 32]>")]
    pub txid: Hash,
    pub unlocked: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BaseResponse {
    pub credits: u64,
    pub status: Status,
    pub top_hash: String,
    pub untrusted: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct GetOIndexesResponse {
    #[serde(flatten)]
    pub base: BaseResponse,
    #[serde(default)]
    pub o_indexes: Vec<u64>,
}

#[derive(Copy, Clone, Debug, Deserialize, PartialEq)]
pub enum Status {
    #[serde(rename = "OK")]
    Ok,
    #[serde(rename = "Failed")]
    Failed,
}

mod monero_serde_hex_block {
    use super::*;
    use monero::consensus::Decodable;
    use serde::{de::Error, Deserialize, Deserializer};
    use std::io::Cursor;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<monero::Block, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex = String::deserialize(deserializer)?;

        let bytes = hex::decode(&hex).map_err(D::Error::custom)?;
        let mut cursor = Cursor::new(bytes);

        let block = monero::Block::consensus_decode(&mut cursor).map_err(D::Error::custom)?;

        Ok(block)
    }
}

mod monero_serde_hex_transaction {
    use super::*;
    use monero::consensus::Decodable;
    use serde::{de::Error, Deserialize, Deserializer};
    use std::io::Cursor;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<monero::Transaction, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex = String::deserialize(deserializer)?;

        let bytes = hex::decode(&hex).map_err(D::Error::custom)?;
        let mut cursor = Cursor::new(bytes);

        let block = monero::Transaction::consensus_decode(&mut cursor).map_err(D::Error::custom)?;

        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryInto;

    #[test]
    fn serialize_get_o_indexes_payload() {
        let payload = GetOIndexesPayload {
            txid: "0bdd2418548da386d9594d2c7245fcdbb5212d3136a3e2170fe25d1c663af9ae"
                .parse()
                .unwrap(),
        };

        let serialized = monero_epee_bin_serde::to_bytes(&payload).unwrap();

        assert_eq!(serialized, vec![
            1, 17, 1, 1, 1, 1, 2, 1, 1, 4, 4, 116, 120, 105, 100, 10, 128, 11, 221, 36, 24, 84,
            141, 163, 134, 217, 89, 77, 44, 114, 69, 252, 219, 181, 33, 45, 49, 54, 163, 226, 23,
            15, 226, 93, 28, 102, 58, 249, 174
        ]);
    }

    #[test]
    fn serialize_get_outs_response() {
        let serialized = vec![
            1, 17, 1, 1, 1, 1, 2, 1, 1, 20, 7, 99, 114, 101, 100, 105, 116, 115, 5, 0, 0, 0, 0, 0,
            0, 0, 0, 4, 111, 117, 116, 115, 140, 8, 20, 6, 104, 101, 105, 103, 104, 116, 5, 160,
            137, 0, 0, 0, 0, 0, 0, 3, 107, 101, 121, 10, 128, 196, 230, 228, 99, 110, 92, 135, 48,
            214, 48, 163, 38, 67, 223, 131, 119, 178, 119, 204, 39, 248, 228, 128, 191, 235, 9,
            141, 208, 244, 146, 77, 183, 4, 109, 97, 115, 107, 10, 128, 125, 48, 19, 21, 95, 237,
            13, 240, 131, 129, 119, 85, 86, 182, 134, 102, 143, 33, 246, 173, 92, 233, 51, 45, 226,
            192, 29, 195, 100, 251, 247, 62, 4, 116, 120, 105, 100, 10, 128, 60, 124, 111, 251,
            212, 227, 37, 68, 131, 236, 210, 49, 243, 39, 129, 186, 167, 99, 109, 134, 146, 252,
            16, 126, 143, 104, 113, 31, 209, 240, 138, 10, 8, 117, 110, 108, 111, 99, 107, 101,
            100, 11, 1, 20, 6, 104, 101, 105, 103, 104, 116, 5, 234, 154, 0, 0, 0, 0, 0, 0, 3, 107,
            101, 121, 10, 128, 137, 17, 157, 123, 99, 63, 39, 21, 109, 248, 127, 124, 106, 167,
            225, 212, 162, 87, 103, 140, 12, 181, 82, 53, 237, 227, 208, 140, 19, 195, 32, 214, 4,
            109, 97, 115, 107, 10, 128, 155, 99, 238, 164, 35, 235, 70, 138, 156, 90, 209, 116,
            130, 59, 5, 222, 246, 103, 68, 201, 138, 108, 159, 27, 164, 175, 159, 113, 216, 170,
            94, 185, 4, 116, 120, 105, 100, 10, 128, 74, 167, 203, 241, 95, 83, 105, 214, 153, 8,
            60, 211, 169, 90, 84, 254, 129, 90, 198, 167, 177, 191, 19, 228, 43, 101, 147, 38, 231,
            196, 222, 63, 8, 117, 110, 108, 111, 99, 107, 101, 100, 11, 1, 6, 115, 116, 97, 116,
            117, 115, 10, 8, 79, 75, 8, 116, 111, 112, 95, 104, 97, 115, 104, 10, 0, 9, 117, 110,
            116, 114, 117, 115, 116, 101, 100, 11, 0,
        ];

        let out = monero_epee_bin_serde::from_bytes::<GetOutsResponse, _>(serialized).unwrap();

        assert_eq!(out, GetOutsResponse {
            base: BaseResponse {
                credits: 0,
                status: Status::Ok,
                top_hash: "".to_string(),
                untrusted: false
            },
            outs: vec![
                OutKey {
                    height: 35232,
                    key: [
                        196, 230, 228, 99, 110, 92, 135, 48, 214, 48, 163, 38, 67, 223, 131, 119,
                        178, 119, 204, 39, 248, 228, 128, 191, 235, 9, 141, 208, 244, 146, 77, 183
                    ]
                    .try_into()
                    .unwrap(),
                    mask: [
                        125, 48, 19, 21, 95, 237, 13, 240, 131, 129, 119, 85, 86, 182, 134, 102,
                        143, 33, 246, 173, 92, 233, 51, 45, 226, 192, 29, 195, 100, 251, 247, 62
                    ]
                    .try_into()
                    .unwrap(),
                    txid: "3c7c6ffbd4e3254483ecd231f32781baa7636d8692fc107e8f68711fd1f08a0a"
                        .parse()
                        .unwrap(),
                    unlocked: true
                },
                OutKey {
                    height: 39658,
                    key: [
                        137, 17, 157, 123, 99, 63, 39, 21, 109, 248, 127, 124, 106, 167, 225, 212,
                        162, 87, 103, 140, 12, 181, 82, 53, 237, 227, 208, 140, 19, 195, 32, 214
                    ]
                    .try_into()
                    .unwrap(),
                    mask: [
                        155, 99, 238, 164, 35, 235, 70, 138, 156, 90, 209, 116, 130, 59, 5, 222,
                        246, 103, 68, 201, 138, 108, 159, 27, 164, 175, 159, 113, 216, 170, 94,
                        185
                    ]
                    .try_into()
                    .unwrap(),
                    txid: "4aa7cbf15f5369d699083cd3a95a54fe815ac6a7b1bf13e42b659326e7c4de3f"
                        .parse()
                        .unwrap(),
                    unlocked: true
                },
            ]
        });
    }
}
