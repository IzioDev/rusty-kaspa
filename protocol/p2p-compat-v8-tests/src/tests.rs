use bytes::BytesMut;
use kaspa_p2p_lib::pb as v9;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};
use std::fs;

fn descriptor_pool() -> DescriptorPool {
    let desc_bytes = fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/src/p2p_v8.desc")).expect("descriptor written at build time");
    DescriptorPool::decode(desc_bytes.as_slice()).expect("valid descriptor set")
}

#[test]
fn compat_v8_ignores_v9_relay_fields_on_version() {
    let mut v9_ver = v9::VersionMessage::default();
    v9_ver.services = 1;
    v9_ver.relay_port = 18_111;

    let mut buf = BytesMut::with_capacity(128);
    v9_ver.encode(&mut buf).unwrap();

    let pool = descriptor_pool();
    let desc = pool.get_message_by_name("protowire.VersionMessage").unwrap();
    assert!(desc.get_field_by_name("relayPort").is_none());
    assert!(desc.get_field_by_name("services").is_none());

    // Decode succeeds even though v8 schema is unaware of the new fields.
    let _ = DynamicMessage::decode(desc, buf.freeze()).unwrap();
}

#[test]
fn compat_v9_handles_v8_version_without_relay_fields() {
    let pool = descriptor_pool();
    let desc = pool.get_message_by_name("protowire.VersionMessage").unwrap();
    let v8_msg = DynamicMessage::new(desc);

    let mut buf = BytesMut::new();
    v8_msg.encode(&mut buf).unwrap();

    let parsed = v9::VersionMessage::decode(buf.freeze()).unwrap();
    assert_eq!(parsed.services, 0);
    assert_eq!(parsed.relay_port, 0);
}

#[test]
fn compat_v8_address_gossip_survives_relay_metadata() {
    let mut v9_addr = v9::NetAddress::default();
    v9_addr.services = 1;
    v9_addr.relay_port = 18_111;

    let mut buf = BytesMut::new();
    v9_addr.encode(&mut buf).unwrap();

    let pool = descriptor_pool();
    let desc = pool.get_message_by_name("protowire.NetAddress").unwrap();
    let _ = DynamicMessage::decode(desc, buf.freeze()).unwrap();
}
