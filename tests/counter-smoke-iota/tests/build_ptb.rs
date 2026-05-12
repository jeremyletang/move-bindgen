//! Offline PTB-construction smoke test.
//!
//! Builds a transaction by calling generated functions, registers the
//! objects they reference, finalizes the builder, and inspects the resulting
//! `ProgrammableTransaction` to confirm the commands are well-formed.

use counter_iota_rs::counter;
use move_bindgen_runtime::*;

fn fake_object_id(byte: u8) -> ObjectId {
    let mut bytes = [0u8; 32];
    bytes[31] = byte;
    ObjectId::from(Address::new(bytes))
}

fn fake_object_ref(byte: u8) -> ObjectReference {
    ObjectReference {
        object_id: fake_object_id(byte),
        version: Version::from_u64(1),
        digest: iota_sdk_types::Digest::ZERO,
    }
}

#[tokio::test]
async fn increment_call_finalizes_into_a_well_formed_ptb() {
    let sender = Address::ZERO;

    // Register the package's on-chain address against its generated
    // `Package` marker. Generated `move_call*` callsites resolve the
    // address through this. Stand-in value for the smoke — real usage
    // would use a published-at from the deployment.
    let package_addr = Address::new({
        let mut b = [0u8; 32];
        b[31] = 0xAB;
        b
    });
    let mut ptb = PtbBuilder::new(sender).with_package::<counter_iota_rs::Package>(package_addr);

    // Counter is shared, AdminCap is owned. Register both before any call.
    let counter_id = fake_object_id(0xC0);
    let admin_id = fake_object_id(0xAD);
    ptb.register_shared(counter_id, /*initial_v=*/ 1, /*mutable=*/ true);
    ptb.register_owned(admin_id, fake_object_ref(0xAD));

    // Build three calls — bare ObjectIds + a Pure u64.
    counter::increment(&mut ptb, counter_id, 5_u64).await;
    counter::value(&mut ptb, counter_id).await;
    counter::reset(&mut ptb, admin_id, counter_id).await;

    // Gas setup so finish() succeeds.
    ptb.inner.gas_price(1_000);
    ptb.inner.gas_budget(1_000_000);
    ptb.inner
        .input(Input::ImmutableOrOwned(fake_object_ref(0x6A))); // gas coin (registered as a regular input here is fine for the smoke)

    let tx = ptb.inner.finish().expect("PTB should finalize");

    // Pull out the ProgrammableTransaction kind to inspect commands.
    use iota_sdk_types::{Argument, Command, Transaction, TransactionKind};
    let kind = match tx {
        Transaction::V1(v1) => v1.kind,
        _ => panic!("non-V1 Transaction"),
    };
    let pt = match kind {
        TransactionKind::ProgrammableTransaction(pt) => pt,
        _ => panic!("expected ProgrammableTransaction"),
    };

    assert_eq!(pt.commands.len(), 3, "three MoveCalls expected");

    // Each command should be a MoveCall into our generated package.
    for (i, cmd) in pt.commands.iter().enumerate() {
        let mc = match cmd {
            Command::MoveCall(mc) => mc,
            _ => panic!("command #{i} is not a MoveCall: {cmd:?}"),
        };
        assert_eq!(
            mc.package,
            ObjectId::from(package_addr),
            "package mismatch on command #{i}",
        );
        assert_eq!(mc.module.as_str(), "counter");
    }

    // Specifically: command #0 is increment(counter, 5).
    let inc = match &pt.commands[0] {
        Command::MoveCall(mc) => mc,
        _ => unreachable!(),
    };
    assert_eq!(inc.function.as_str(), "increment");
    assert!(inc.type_arguments.is_empty());
    assert_eq!(inc.arguments.len(), 2);
    // The second arg is a Pure (u64) input.
    assert!(matches!(inc.arguments[1], Argument::Input(_)));
}
