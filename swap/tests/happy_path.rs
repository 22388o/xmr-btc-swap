use swap::protocol::{alice, bob};
use tokio::join;

pub mod testutils;

/// Run the following tests with RUST_MIN_STACK=10000000

#[tokio::test]
async fn happy_path() {
    testutils::setup_test(|mut ctx| async move {
        let alice_swap = ctx.new_swap_as_alice().await;
        let bob_swap = ctx.new_swap_as_bob().await;

        let alice = alice::run(alice_swap);

        let bob = bob::run(bob_swap);
        let (alice_state, bob_state) = join!(alice, bob);

        ctx.assert_alice_redeemed(alice_state.unwrap()).await;
        ctx.assert_bob_redeemed(bob_state.unwrap()).await;
    })
    .await;
}