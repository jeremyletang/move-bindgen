//! Offline PTB-construction smoke test for the Sui-flavoured counter.
//!
//! Builds a transaction by calling generated functions, registers the
//! package's runtime address + a stand-in shared object, and confirms
//! the builder finalizes without error.

use counter_sui_rs::{counter, registry};
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

/// Regression for `BUGS/move-bindgen-string-typetag.md`. See the IOTA
/// twin (`tests/counter-smoke-iota/tests/build_ptb.rs`) for the full
/// rationale — same fix, same shape, mirrored for the Sui flavour.
#[tokio::test]
async fn stdlib_string_generics_produce_correct_type_tags() {
    let sender = Address::ZERO;
    let package_addr = fake_addr(0xAB);
    let mut ptb = PtbBuilder::new(sender).with_package::<counter_sui_rs::Package>(package_addr);

    let _ = registry::new_holder::<String>(&mut ptb).await;
    let _ = registry::new_holder::<AsciiString>(&mut ptb).await;

    // Sui's `TransactionBuilder` keeps the command list as a private
    // field — finalize the tx and inspect the resolved kind instead.
    ptb.inner.tx.set_sender(sender);
    ptb.inner.tx.set_gas_price(1_000);
    ptb.inner.tx.set_gas_budget(1_000_000);
    ptb.inner
        .tx
        .add_gas_objects([sui_transaction_builder::ObjectInput::owned(
            fake_addr(0x6A),
            1,
            sui_sdk_types::Digest::ZERO,
        )]);

    let tx = ptb.inner.tx.try_build().expect("PTB should finalize");
    let cmds = match &tx.kind {
        sui_sdk_types::TransactionKind::ProgrammableTransaction(pt) => &pt.commands,
        other => panic!("expected ProgrammableTransaction, got {other:?}"),
    };
    assert_eq!(cmds.len(), 2);

    let extract_tag = |i: usize| -> TypeTag {
        let mc = match &cmds[i] {
            sui_sdk_types::Command::MoveCall(mc) => mc,
            other => panic!("command #{i} is not a MoveCall: {other:?}"),
        };
        assert_eq!(mc.function.as_str(), "new_holder");
        assert_eq!(mc.type_arguments.len(), 1);
        mc.type_arguments[0].clone()
    };

    match extract_tag(0) {
        TypeTag::Struct(s) => {
            assert_eq!(*s.address(), MOVE_STDLIB_ADDRESS);
            assert_eq!(s.module().as_str(), "string");
            assert_eq!(s.name().as_str(), "String");
        }
        other => panic!("new_holder::<String> type_args[0] must be Struct(0x1::string::String), got {other:?}"),
    }

    match extract_tag(1) {
        TypeTag::Struct(s) => {
            assert_eq!(*s.address(), MOVE_STDLIB_ADDRESS);
            assert_eq!(s.module().as_str(), "ascii");
            assert_eq!(s.name().as_str(), "String");
        }
        other => panic!("new_holder::<AsciiString> type_args[0] must be Struct(0x1::ascii::String), got {other:?}"),
    }
}
