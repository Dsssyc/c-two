//! Unit tests for c2-wire codec.
//!
//! Tests verify round-trip encoding/decoding and canonical cross-language
//! wire compatibility fixtures.

mod frame_tests {
    use crate::flags::*;
    use crate::frame::*;

    #[test]
    fn encode_decode_roundtrip() {
        let payload = b"hello world";
        let encoded = encode_frame(42, FLAG_BUDDY | FLAG_CALL_V2, payload);
        assert_eq!(encoded.len(), HEADER_SIZE + payload.len());

        let (hdr, decoded_payload) = decode_frame(&encoded).unwrap();
        assert_eq!(hdr.request_id, 42);
        assert_eq!(hdr.flags, FLAG_BUDDY | FLAG_CALL_V2);
        assert_eq!(decoded_payload, payload);
        assert!(hdr.is_buddy());
        assert!(hdr.is_call_v2());
        assert!(!hdr.is_response());
    }

    #[test]
    fn encode_decode_empty_payload() {
        let encoded = encode_frame(0, 0, &[]);
        let (hdr, payload) = decode_frame(&encoded).unwrap();
        assert_eq!(hdr.request_id, 0);
        assert_eq!(hdr.flags, 0);
        assert!(payload.is_empty());
        assert_eq!(hdr.total_len, 12); // 8B rid + 4B flags
    }

    #[test]
    fn decode_total_len_basic() {
        let buf = 42u32.to_le_bytes();
        let (total_len, rest) = decode_total_len(&buf).unwrap();
        assert_eq!(total_len, 42);
        assert!(rest.is_empty());
    }

    #[test]
    fn decode_truncated() {
        let encoded = encode_frame(1, 0, b"data");
        // Truncate the frame
        let result = decode_frame(&encoded[..10]);
        assert!(result.is_err());
    }

    #[test]
    fn header_predicates() {
        let hdr = FrameHeader {
            total_len: 12,
            request_id: 1,
            flags: FLAG_RESPONSE | FLAG_REPLY_V2 | FLAG_BUDDY,
        };
        assert!(hdr.is_response());
        assert!(hdr.is_reply_v2());
        assert!(hdr.is_buddy());
        assert!(!hdr.is_call_v2());
        assert!(!hdr.is_handshake());
        assert!(!hdr.is_ctrl());
    }

    #[test]
    fn total_len_matches_canonical_frame_format() {
        // Canonical frame header layout: `<IQI` → 16 bytes
        // total_len = 12 + payload_len
        // Frame = [4B total_len][8B rid][4B flags][payload]
        let payload = b"test";
        let encoded = encode_frame(100, FLAG_CALL_V2, payload);

        // Check total_len value
        let total_len = u32::from_le_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        assert_eq!(total_len, 12 + 4); // 8 + 4 + payload_len

        // Check request_id
        let rid = u64::from_le_bytes([
            encoded[4],
            encoded[5],
            encoded[6],
            encoded[7],
            encoded[8],
            encoded[9],
            encoded[10],
            encoded[11],
        ]);
        assert_eq!(rid, 100);

        // Check flags
        let flags = u32::from_le_bytes([encoded[12], encoded[13], encoded[14], encoded[15]]);
        assert_eq!(flags, FLAG_CALL_V2);
    }
}

mod buddy_tests {
    use crate::buddy::*;

    #[test]
    fn roundtrip() {
        let bp = BuddyPayload {
            seg_idx: 3,
            offset: 65536,
            data_size: 1024,
            is_dedicated: false,
        };
        let encoded = encode_buddy_payload(&bp);
        assert_eq!(encoded.len(), BUDDY_PAYLOAD_SIZE);

        let (decoded, consumed) = decode_buddy_payload(&encoded).unwrap();
        assert_eq!(consumed, BUDDY_PAYLOAD_SIZE);
        assert_eq!(decoded, bp);
    }

    #[test]
    fn dedicated_flag() {
        let bp = BuddyPayload {
            seg_idx: 0,
            offset: 0,
            data_size: 256,
            is_dedicated: true,
        };
        let encoded = encode_buddy_payload(&bp);
        assert_eq!(encoded[10], BUDDY_FLAG_DEDICATED);

        let (decoded, _) = decode_buddy_payload(&encoded).unwrap();
        assert!(decoded.is_dedicated);
    }

    #[test]
    fn canonical_buddy_payload_layout() {
        // Canonical buddy payload layout: `<HII B` → H(2) + I(4) + I(4) + B(1) = 11
        assert_eq!(BUDDY_PAYLOAD_SIZE, 11);

        let bp = BuddyPayload {
            seg_idx: 1,
            offset: 0x00010000,
            data_size: 0x00000400,
            is_dedicated: false,
        };
        let encoded = encode_buddy_payload(&bp);
        // seg_idx=1 LE → [0x01, 0x00]
        assert_eq!(encoded[0], 0x01);
        assert_eq!(encoded[1], 0x00);
        // offset=65536 LE → [0x00, 0x00, 0x01, 0x00]
        assert_eq!(encoded[2], 0x00);
        assert_eq!(encoded[3], 0x00);
        assert_eq!(encoded[4], 0x01);
        assert_eq!(encoded[5], 0x00);
    }
}

mod control_tests {
    use crate::control::*;

    #[test]
    fn call_control_roundtrip() {
        let encoded = encode_call_control("grid", 42).unwrap();
        let (decoded, consumed) = decode_call_control(&encoded, 0).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.route_name, "grid");
        assert_eq!(decoded.method_idx, 42);
    }

    #[test]
    fn call_control_empty_name() {
        let encoded = encode_call_control("", 0).unwrap();
        assert_eq!(encoded.len(), 3); // 1B name_len=0 + 2B idx
        let (decoded, consumed) = decode_call_control(&encoded, 0).unwrap();
        assert_eq!(consumed, 3);
        assert_eq!(decoded.route_name, "");
        assert_eq!(decoded.method_idx, 0);
    }

    #[test]
    fn call_control_with_offset() {
        let mut buf = vec![0xAA, 0xBB]; // prefix
        buf.extend_from_slice(&encode_call_control("net", 7).unwrap());
        let (decoded, consumed) = decode_call_control(&buf, 2).unwrap();
        assert_eq!(decoded.route_name, "net");
        assert_eq!(decoded.method_idx, 7);
        assert_eq!(consumed, 1 + 3 + 2); // name_len + "net" + idx
    }

    #[test]
    fn reply_control_success_roundtrip() {
        let encoded = encode_reply_control(&ReplyControl::Success);
        assert_eq!(encoded, &[STATUS_SUCCESS]);
        let (decoded, consumed) = decode_reply_control(&encoded, 0).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(decoded, ReplyControl::Success);
    }

    #[test]
    fn reply_control_error_roundtrip() {
        let err = b"3:test error".to_vec();
        let encoded = encode_reply_control(&ReplyControl::Error(err.clone()));
        let (decoded, consumed) = decode_reply_control(&encoded, 0).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, ReplyControl::Error(err));
    }

    #[test]
    fn reply_control_error_empty_data() {
        let encoded = encode_reply_control(&ReplyControl::Error(vec![]));
        // status=1, error_len=0 → [0x01, 0x00, 0x00, 0x00, 0x00]
        assert_eq!(encoded.len(), 5);
        let (decoded, consumed) = decode_reply_control(&encoded, 0).unwrap();
        assert_eq!(consumed, 5);
        assert_eq!(decoded, ReplyControl::Error(vec![]));
    }

    #[test]
    fn reply_control_route_not_found_roundtrip() {
        let encoded = encode_reply_control(&ReplyControl::RouteNotFound("grid".into()));
        assert_eq!(encoded[0], STATUS_ROUTE_NOT_FOUND);
        let (decoded, consumed) = decode_reply_control(&encoded, 0).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, ReplyControl::RouteNotFound("grid".into()));
    }

    #[test]
    fn reply_control_route_not_found_rejects_truncated_name() {
        let mut encoded = vec![STATUS_ROUTE_NOT_FOUND];
        encoded.extend_from_slice(&8u32.to_le_bytes());
        encoded.extend_from_slice(b"grid");

        assert!(decode_reply_control(&encoded, 0).is_err());
    }

    #[test]
    fn reply_control_invalid_status() {
        let buf = [0xFF];
        let result = decode_reply_control(&buf, 0);
        assert!(result.is_err());
    }

    #[test]
    fn canonical_call_control_fixture_matches() {
        // Canonical call-control fixture for route `grid`, method index 5
        // = [4] + b"grid" + struct.pack('<H', 5)
        // = [0x04, 0x67, 0x72, 0x69, 0x64, 0x05, 0x00]
        let expected = vec![0x04, 0x67, 0x72, 0x69, 0x64, 0x05, 0x00];
        let encoded = encode_call_control("grid", 5).unwrap();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn call_control_rejects_route_name_longer_than_one_byte_length() {
        let long_name = "x".repeat(MAX_CALL_ROUTE_NAME_BYTES + 1);
        let err = encode_call_control(&long_name, 0).unwrap_err();
        assert!(err.to_string().contains("route_name"));
    }
}

mod handshake_tests {
    use crate::handshake::*;

    fn test_identity() -> ServerIdentity {
        ServerIdentity {
            server_id: "test-server".to_string(),
            server_instance_id: "test-instance".to_string(),
        }
    }

    #[test]
    fn client_handshake_roundtrip() {
        let segments = vec![
            ("seg0".into(), 268_435_456u32),
            ("seg1".into(), 268_435_456u32),
        ];
        let encoded = encode_client_handshake(&segments, CAP_CALL_V2 | CAP_METHOD_IDX, "").unwrap();
        let decoded = decode_handshake(&encoded).unwrap();

        assert_eq!(decoded.prefix, "");
        assert_eq!(decoded.segments.len(), 2);
        assert_eq!(decoded.segments[0].0, "seg0");
        assert_eq!(decoded.segments[0].1, 268_435_456);
        assert_eq!(decoded.capability_flags, CAP_CALL_V2 | CAP_METHOD_IDX);
        assert_eq!(decoded.server_identity, None);
        assert!(decoded.routes.is_empty());
    }

    #[test]
    fn server_handshake_roundtrip_includes_server_identity() {
        let routes = vec![RouteInfo {
            name: "grid".to_string(),
            methods: vec![MethodEntry {
                name: "ping".to_string(),
                index: 0,
            }],
        }];
        let identity = ServerIdentity {
            server_id: "grid-server".to_string(),
            server_instance_id: "inst-001".to_string(),
        };

        let encoded = encode_server_handshake(
            &[],
            CAP_CALL_V2 | CAP_METHOD_IDX,
            &routes,
            "/cc3rtest",
            &identity,
        )
        .expect("server handshake encodes");
        let decoded = decode_handshake(&encoded).expect("server handshake decodes");

        assert_eq!(decoded.server_identity.as_ref(), Some(&identity));
        assert_eq!(decoded.routes, routes);
    }

    #[test]
    fn server_handshake_rejects_trailing_bytes() {
        let mut encoded = encode_server_handshake(
            &[],
            CAP_CALL_V2 | CAP_METHOD_IDX,
            &[],
            "/cc3rtest",
            &test_identity(),
        )
        .expect("server handshake encodes");
        encoded.push(0xff);

        let err = decode_handshake(&encoded).expect_err("trailing byte must be rejected");
        assert!(err.to_string().contains("trailing bytes"));
    }

    #[test]
    fn client_handshake_has_no_server_identity() {
        let encoded = encode_client_handshake(&[], CAP_CALL_V2, "/cc3ctest")
            .expect("client handshake encodes");
        let decoded = decode_handshake(&encoded).expect("client handshake decodes");

        assert_eq!(decoded.server_identity, None);
        assert!(decoded.routes.is_empty());
    }

    #[test]
    fn server_handshake_roundtrip() {
        let segments = vec![("srv_seg0".into(), 134_217_728u32)];
        let routes = vec![
            RouteInfo {
                name: "grid".into(),
                methods: vec![
                    MethodEntry {
                        name: "hello".into(),
                        index: 0,
                    },
                    MethodEntry {
                        name: "subdivide_grids".into(),
                        index: 1,
                    },
                    MethodEntry {
                        name: "get_grid_infos".into(),
                        index: 2,
                    },
                ],
            },
            RouteInfo {
                name: "counter".into(),
                methods: vec![
                    MethodEntry {
                        name: "get".into(),
                        index: 0,
                    },
                    MethodEntry {
                        name: "increment".into(),
                        index: 1,
                    },
                ],
            },
        ];
        let identity = test_identity();
        let encoded =
            encode_server_handshake(&segments, CAP_CALL_V2, &routes, "", &identity).unwrap();
        let decoded = decode_handshake(&encoded).unwrap();

        assert_eq!(decoded.segments.len(), 1);
        assert_eq!(decoded.capability_flags, CAP_CALL_V2);
        assert_eq!(decoded.server_identity.as_ref(), Some(&identity));
        assert_eq!(decoded.routes.len(), 2);

        let grid = &decoded.routes[0];
        assert_eq!(grid.name, "grid");
        assert_eq!(grid.methods.len(), 3);
        assert_eq!(grid.methods[0].name, "hello");
        assert_eq!(grid.methods[0].index, 0);
        assert_eq!(grid.methods[2].name, "get_grid_infos");
        assert_eq!(grid.methods[2].index, 2);

        let counter = &decoded.routes[1];
        assert_eq!(counter.name, "counter");
        assert_eq!(counter.methods.len(), 2);
    }

    #[test]
    fn server_handshake_rejects_overlong_route_name() {
        let routes = vec![RouteInfo {
            name: "x".repeat(MAX_HANDSHAKE_NAME_BYTES + 1),
            methods: vec![],
        }];

        let err =
            encode_server_handshake(&[], CAP_CALL_V2, &routes, "", &test_identity()).unwrap_err();
        assert!(err.to_string().contains("route name"));
    }

    #[test]
    fn server_handshake_rejects_too_many_methods() {
        let routes = vec![RouteInfo {
            name: "grid".into(),
            methods: (0..=MAX_METHODS)
                .map(|i| MethodEntry {
                    name: format!("m{i}"),
                    index: i as u16,
                })
                .collect(),
        }];

        let err =
            encode_server_handshake(&[], CAP_CALL_V2, &routes, "", &test_identity()).unwrap_err();
        assert!(err.to_string().contains("method count"));
    }

    #[test]
    fn wrong_version() {
        let buf = [4, 0, 0]; // version 4
        let result = decode_handshake(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn empty_handshake() {
        // Version 7, prefix_len=0, 0 segments, cap_flags=0
        let buf = [7, 0, 0, 0, 0, 0];
        let decoded = decode_handshake(&buf).unwrap();
        assert_eq!(decoded.prefix, "");
        assert!(decoded.segments.is_empty());
        assert_eq!(decoded.capability_flags, 0);
        assert_eq!(decoded.server_identity, None);
        assert!(decoded.routes.is_empty());
    }
}

// ── Cross-language compatibility tests ───────────────────────────────────
// Canonical wire compatibility fixtures shared by all SDK bindings.

mod cross_lang_tests {
    use crate::buddy::*;
    use crate::control::*;
    use crate::frame::*;
    use crate::handshake::*;

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn canonical_frame_fixture_decodes() {
        let bytes = hex_to_bytes("180000003930000000000000c2010000746573745f7061796c6f6164");
        let (hdr, payload) = decode_frame(&bytes).unwrap();
        assert_eq!(hdr.request_id, 12345);
        assert_eq!(hdr.flags, 0x1C2);
        assert_eq!(payload, b"test_payload");
    }

    #[test]
    fn canonical_call_control_fixture_decodes() {
        let bytes = hex_to_bytes("0568656c6c6f0700");
        let (ctrl, consumed) = decode_call_control(&bytes, 0).unwrap();
        assert_eq!(ctrl.route_name, "hello");
        assert_eq!(ctrl.method_idx, 7);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn canonical_empty_call_control_fixture_decodes() {
        let bytes = hex_to_bytes("000000");
        let (ctrl, consumed) = decode_call_control(&bytes, 0).unwrap();
        assert_eq!(ctrl.route_name, "");
        assert_eq!(ctrl.method_idx, 0);
        assert_eq!(consumed, 3);
    }

    #[test]
    fn canonical_reply_success_fixture_decodes() {
        let bytes = hex_to_bytes("00");
        let (ctrl, consumed) = decode_reply_control(&bytes, 0).unwrap();
        assert_eq!(ctrl, ReplyControl::Success);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn canonical_reply_error_fixture_decodes() {
        let bytes = hex_to_bytes("010c000000333a74657374206572726f72");
        let (ctrl, consumed) = decode_reply_control(&bytes, 0).unwrap();
        match ctrl {
            ReplyControl::Error(data) => {
                assert_eq!(data, b"3:test error");
            }
            _ => panic!("expected error"),
        }
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn canonical_buddy_payload_fixture_decodes() {
        let bytes = hex_to_bytes("0200001000000002000000");
        let (bp, consumed) = decode_buddy_payload(&bytes).unwrap();
        assert_eq!(bp.seg_idx, 2);
        assert_eq!(bp.offset, 4096);
        assert_eq!(bp.data_size, 512);
        assert!(!bp.is_dedicated);
        assert_eq!(consumed, BUDDY_PAYLOAD_SIZE);
    }

    #[test]
    fn canonical_client_handshake_fixture_decodes() {
        // v7: [07][00 prefix_len][01 00 seg_count][00 00 00 10 size][04 seg0][03 00 caps]
        let bytes = hex_to_bytes("070001000000001004736567300300");
        let hs = decode_handshake(&bytes).unwrap();
        assert_eq!(hs.prefix, "");
        assert_eq!(hs.segments.len(), 1);
        assert_eq!(hs.segments[0].0, "seg0");
        assert_eq!(hs.segments[0].1, 268_435_456);
        assert_eq!(hs.capability_flags, CAP_CALL_V2 | CAP_METHOD_IDX);
        assert_eq!(hs.server_identity, None);
        assert!(hs.routes.is_empty());
    }

    #[test]
    fn canonical_server_handshake_fixture_decodes() {
        // v7: client handshake prefix, server identity, then route table.
        let bytes = hex_to_bytes(
            "0700010000000008047372763003000b7365727665722d6772696409696e73742d677269640100046772696402000568656c6c6f0000036164640100",
        );
        let hs = decode_handshake(&bytes).unwrap();
        assert_eq!(hs.prefix, "");
        assert_eq!(hs.segments.len(), 1);
        assert_eq!(hs.segments[0].0, "srv0");
        assert_eq!(hs.segments[0].1, 134_217_728);
        assert_eq!(hs.capability_flags, CAP_CALL_V2 | CAP_METHOD_IDX);
        assert_eq!(
            hs.server_identity.as_ref(),
            Some(&ServerIdentity {
                server_id: "server-grid".into(),
                server_instance_id: "inst-grid".into(),
            })
        );
        assert_eq!(hs.routes.len(), 1);
        assert_eq!(hs.routes[0].name, "grid");
        assert_eq!(hs.routes[0].methods.len(), 2);
        assert_eq!(hs.routes[0].methods[0].name, "hello");
        assert_eq!(hs.routes[0].methods[0].index, 0);
        assert_eq!(hs.routes[0].methods[1].name, "add");
        assert_eq!(hs.routes[0].methods[1].index, 1);
    }

    #[test]
    fn rust_encode_matches_canonical_call_control_fixture() {
        let encoded = encode_call_control("hello", 7).unwrap();
        let expected = hex_to_bytes("0568656c6c6f0700");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn rust_encode_matches_canonical_reply_success_fixture() {
        let encoded = encode_reply_control(&ReplyControl::Success);
        assert_eq!(encoded, hex_to_bytes("00"));
    }

    #[test]
    fn rust_encode_matches_canonical_reply_error_fixture() {
        let err = b"3:test error".to_vec();
        let encoded = encode_reply_control(&ReplyControl::Error(err));
        let expected = hex_to_bytes("010c000000333a74657374206572726f72");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn rust_encode_matches_canonical_buddy_payload_fixture() {
        let bp = BuddyPayload {
            seg_idx: 2,
            offset: 4096,
            data_size: 512,
            is_dedicated: false,
        };
        let encoded = encode_buddy_payload(&bp);
        let expected = hex_to_bytes("0200001000000002000000");
        assert_eq!(encoded.as_slice(), expected.as_slice());
    }

    #[test]
    fn rust_encode_matches_canonical_client_handshake_fixture() {
        let segments = vec![("seg0".into(), 268_435_456u32)];
        let encoded = encode_client_handshake(&segments, CAP_CALL_V2 | CAP_METHOD_IDX, "").unwrap();
        // v7: [07][00 prefix_len][01 00 seg_count][00 00 00 10 size][04 name_len][seg0][03 00 caps]
        let expected = hex_to_bytes("070001000000001004736567300300");
        assert_eq!(encoded, expected);
    }

    #[test]
    fn rust_encode_matches_canonical_server_handshake_fixture() {
        let segments = vec![("srv0".into(), 134_217_728u32)];
        let routes = vec![RouteInfo {
            name: "grid".into(),
            methods: vec![
                MethodEntry {
                    name: "hello".into(),
                    index: 0,
                },
                MethodEntry {
                    name: "add".into(),
                    index: 1,
                },
            ],
        }];
        let identity = ServerIdentity {
            server_id: "server-grid".into(),
            server_instance_id: "inst-grid".into(),
        };
        let encoded = encode_server_handshake(
            &segments,
            CAP_CALL_V2 | CAP_METHOD_IDX,
            &routes,
            "",
            &identity,
        )
        .unwrap();
        // v7: [07][00 prefix_len] then segments/caps, identity, and route table.
        let expected = hex_to_bytes(
            "0700010000000008047372763003000b7365727665722d6772696409696e73742d677269640100046772696402000568656c6c6f0000036164640100",
        );
        assert_eq!(encoded, expected);
    }
}
