//! Offline PTB-construction smoke test for the Sui-flavoured counter.
//!
//! Builds a transaction by calling generated functions, registers the
//! package's runtime address + a stand-in shared object, and confirms
//! the builder finalizes without error.

use counter_sui_rs::counter;
use move_bindgen_runtime::*;

fn fake_addr(byte: u8) -> Address {
    let mut bytes = [0u8; 32];
    bytes[31] = byte;
    Address::new(bytes)
}

#[tokio::test]
async fn increment_call_finalizes_into_a_well_formed_ptb() {
    let sender = Address::ZERO;
    let package_addr = fake_addr(0xAB);

    let mut ptb = PtbBuilder::new(sender).with_package::<counter_sui_rs::Package>(package_addr);

    // Counter is shared in `counter_sui::counter::create`. Register
    // the object id so generated `move_call*` calls can resolve it.
    let counter_id = fake_addr(0xC0);
    ptb.register_shared(counter_id, /*initial_v=*/ 1);

    counter::increment(&mut ptb, counter_id, 5_u64).await;
    counter::value(&mut ptb, counter_id).await;

    // Snapshot returns `(u64, u64)`: exercises `move_call_n`.
    let (_v, _t) = counter::snapshot(&mut ptb, counter_id).await;
}
