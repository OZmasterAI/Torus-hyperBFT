// Frozen 6a51413 / 080c4fa encoders: independent from production helpers.
use super::*;

fn order_params(i: usize) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: u64::MAX - i as u64,
        is_buy: i % 2 == 0,
        price: FixedPoint::from_raw(if i % 2 == 0 { i128::MIN } else { i128::MAX }),
        quantity: FixedPoint::from_raw(i as i128 - 17),
        order_type: match i % 4 {
            0 => OrderType::Limit,
            1 => OrderType::Market,
            2 => OrderType::StopMarket {
                trigger: FixedPoint::MIN,
            },
            _ => OrderType::StopLimit {
                trigger: FixedPoint::MAX,
                limit: FixedPoint::MIN,
            },
        },
        time_in_force: match (i / 4) % 4 {
            0 => TimeInForce::GTC,
            1 => TimeInForce::IOC,
            2 => TimeInForce::FOK,
            _ => TimeInForce::PostOnly,
        },
        reduce_only: (i / 16) % 2 == 0,
        client_order_id: if (i / 32) % 2 == 0 {
            None
        } else {
            Some(u64::MAX - i as u64)
        },
    }
}

fn actions() -> Vec<NativeAction> {
    let params = MarketParams {
        tick_size: FixedPoint::MIN,
        lot_size: FixedPoint::MAX,
        max_leverage: u32::MAX,
        maintenance_margin_bps: 123,
        max_funding_rate_bps: 456,
    };
    let listing = MarketListing {
        base_asset: "é\0base".into(),
        quote_asset: "資産".into(),
        tick_size: FixedPoint::MAX,
        lot_size: FixedPoint::MIN,
        max_leverage: 17,
        maintenance_margin_bps: u32::MAX,
    };
    let mut actions: Vec<_> = (0..64)
        .map(|i| NativeAction::PlaceOrder(order_params(i)))
        .collect();
    actions.extend([
        NativeAction::PlaceOrderBatch(vec![]),
        NativeAction::PlaceOrderBatch((0..64).map(order_params).collect()),
        NativeAction::PlaceOrderBatch((0..1024).map(order_params).collect()),
        NativeAction::CancelOrder {
            order_id: u128::MAX,
        },
        NativeAction::CancelAllOrders { market_id: None },
        NativeAction::CancelAllOrders {
            market_id: Some(u64::MAX),
        },
        NativeAction::TransferToPerp { amount: U256::ZERO },
        NativeAction::TransferToSpot { amount: U256::MAX },
        NativeAction::Withdraw {
            amount: U256::MAX,
            to: Address::repeat_byte(3),
        },
        NativeAction::Delegate {
            validator: Address::repeat_byte(4),
            amount: U256::MAX,
        },
        NativeAction::Undelegate {
            validator: Address::repeat_byte(5),
            amount: U256::from(7),
        },
        NativeAction::PermanentStake { amount: U256::MAX },
        NativeAction::ClaimRewards,
        NativeAction::SubmitOraclePrices(OracleSubmission {
            prices: vec![],
            timestamp: 0,
        }),
        NativeAction::SubmitOraclePrices(OracleSubmission {
            prices: vec![(u64::MAX, FixedPoint::MIN), (0, FixedPoint::MAX)],
            timestamp: u64::MAX,
        }),
        NativeAction::RegisterValidator {
            pubkey: PublicKey([7; 32]),
            commission: u16::MAX,
        },
        NativeAction::UpdateCommission { new_rate: u16::MAX },
        NativeAction::JailVote {
            target: Address::repeat_byte(8),
        },
        NativeAction::UnjailSelf,
        NativeAction::RotateValidatorKey {
            new_pubkey: PublicKey([9; 32]),
        },
        NativeAction::RevokeSession {
            session_pubkey: [10; 32],
        },
        NativeAction::UpdateMarketParams {
            market_id: u64::MAX,
            params: params.clone(),
        },
        NativeAction::ListMarket(listing.clone()),
        NativeAction::DelistMarket {
            market_id: u64::MAX,
        },
        NativeAction::TopUpSelfStake { amount: U256::MAX },
    ]);
    for new_price in [None, Some(FixedPoint::MIN)] {
        for new_qty in [None, Some(FixedPoint::MAX)] {
            actions.push(NativeAction::ModifyOrder {
                order_id: u128::MAX,
                new_price,
                new_qty,
            });
        }
    }
    for scope in [
        SessionScope::Trading,
        SessionScope::TransfersOnly,
        SessionScope::Full,
    ] {
        actions.push(NativeAction::CreateSession {
            session_pubkey: [11; 32],
            expiry: u64::MAX,
            scope,
        });
    }
    for option in [VoteOption::Yes, VoteOption::No, VoteOption::Abstain] {
        actions.push(NativeAction::Vote {
            proposal_id: u64::MAX,
            option,
        });
    }
    for action in [
        ProposalAction::UpdateMarketParams {
            market_id: u64::MAX,
            params,
        },
        ProposalAction::ListMarket(listing),
        ProposalAction::DelistMarket {
            market_id: u64::MAX,
        },
        ProposalAction::ParameterChange {
            key: "\0keyé".into(),
            value: "日本語".into(),
        },
        ProposalAction::ValidatorRegistration {
            candidate: Address::repeat_byte(12),
        },
    ] {
        actions.push(NativeAction::SubmitProposal(Proposal {
            title: "proposal\0é".into(),
            description: "説明".into(),
            action,
        }));
    }
    actions
}

fn signed(action: NativeAction, session: bool, nonce: u64) -> SignedNativeAction {
    SignedNativeAction {
        action,
        nonce,
        signature: if session {
            ActionSignature::Session {
                session_pubkey: [17; 32],
                sig: Ed25519Sig([29; 64]),
            }
        } else {
            ActionSignature::Eip712(Signature {
                v: 28,
                r: [31; 32],
                s: [43; 32],
            })
        },
    }
}

#[test]
fn append_canonical_bytes_matches_frozen_encoder_for_all_variants() {
    let mut tags = std::collections::BTreeSet::new();
    for action in actions() {
        let expected = legacy_canonical_bytes(&action);
        tags.insert(expected[0]);
        assert_eq!(action.canonical_bytes(), expected);
        let mut buffer = vec![0xde, 0xad, 0xbe, 0xef];
        action.append_canonical_bytes(&mut buffer);
        assert_eq!(&buffer[..4], &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(&buffer[4..], expected);
        action.append_canonical_bytes(&mut buffer);
        assert_eq!(&buffer[4 + expected.len()..], expected);
    }
    assert_eq!(
        tags,
        (0u8..=25).collect(),
        "all action tags must be covered"
    );
}

#[test]
fn scratch_hash_matches_frozen_encoder_with_both_signatures_and_nonce_extremes() {
    let mut scratch = vec![0xff; 256];
    for action in actions() {
        for session in [false, true] {
            for nonce in [0, 1, u64::MAX] {
                let envelope = signed(action.clone(), session, nonce);
                let expected = legacy_compute_action_hash(&envelope);
                assert_eq!(
                    compute_action_hash_with_scratch(&envelope, &mut scratch),
                    expected
                );
                assert_eq!(compute_action_hash(&envelope), expected);
                // Confirm the retained preimage contains no stale tail bytes.
                let body = legacy_canonical_bytes(&envelope.action);
                assert_eq!(&scratch[..body.len()], body);
                assert_eq!(&scratch[body.len()..body.len() + 8], nonce.to_be_bytes());
                assert_eq!(
                    scratch.len(),
                    body.len() + 8 + if session { 97 } else { 66 }
                );
            }
        }
    }
}

#[test]
fn scratch_hash_reuses_capacity_across_small_large_small_and_dirty_prefixes() {
    let small = signed(NativeAction::ClaimRewards, false, 1);
    let large = signed(
        NativeAction::PlaceOrderBatch((0..1024).map(order_params).collect()),
        true,
        2,
    );
    let other = signed(NativeAction::CancelAllOrders { market_id: None }, true, 3);
    let mut scratch = vec![0xa5; 13];
    for action in [&small, &large] {
        assert_eq!(
            compute_action_hash_with_scratch(action, &mut scratch),
            legacy_compute_action_hash(action)
        );
    }
    let pointer = scratch.as_ptr();
    let capacity = scratch.capacity();
    for action in [&small, &other, &large, &small] {
        scratch.clear();
        scratch.resize(capacity, 0xa5);
        assert_eq!(
            compute_action_hash_with_scratch(action, &mut scratch),
            legacy_compute_action_hash(action)
        );
        assert_eq!(
            scratch.as_ptr(),
            pointer,
            "warm buffer must not be replaced"
        );
        assert_eq!(
            scratch.capacity(),
            capacity,
            "warm capacity must not shrink or grow"
        );
    }
    // Hash scratch is not an authorization-cache key: session actions remain
    // uncacheable and EIP-712 keeps its distinct, tag-less legacy preimage.
    assert!(verified_cache_key(&other).is_none());
    assert_ne!(
        verified_cache_key(&small).unwrap(),
        compute_action_hash(&small)
    );
}

#[test]
fn compact_block_hash_order_matches_frozen_encoder() {
    let block = TorusBlock {
        header: TorusBlockHeader {
            height: 1,
            parent_hash: B256::ZERO,
            timestamp: 2,
            proposer: Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: Bloom::ZERO,
            evm_gas_used: 0,
            evm_fee_revenue: 0,
            evm_gas_limit: 30_000_000,
            native_action_count: 3,
            evm_tx_count: 1,
            base_fee_per_gas: 1,
            epoch: 0,
            validator_set_hash: B256::ZERO,
            sig_attestation: [0; 64],
        },
        native_actions: vec![
            signed(NativeAction::ClaimRewards, false, 1),
            signed(
                NativeAction::PlaceOrderBatch((0..400).map(order_params).collect()),
                true,
                2,
            ),
            signed(NativeAction::ClaimRewards, true, 3),
        ],
        evm_transactions: vec![vec![1, 2, 3]],
        core_writer_actions: vec![CoreWriterAction::CancelOrder {
            order_id: u128::MAX,
        }],
    };
    let expected = CompactBlock {
        header: block.header.clone(),
        native_action_hashes: block
            .native_actions
            .iter()
            .map(legacy_compute_action_hash)
            .collect(),
        evm_transactions: block.evm_transactions.clone(),
        core_writer_actions: block.core_writer_actions.clone(),
    };
    assert_eq!(
        bincode::serialize(&CompactBlock::from_block(&block)).unwrap(),
        bincode::serialize(&expected).unwrap()
    );
    let empty = TorusBlock {
        native_actions: vec![],
        ..block
    };
    assert!(CompactBlock::from_block(&empty)
        .native_action_hashes
        .is_empty());
}

fn legacy_encode_place_order_params(buf: &mut Vec<u8>, p: &PlaceOrderParams) {
    buf.extend_from_slice(&p.market_id.to_be_bytes());
    buf.push(p.is_buy as u8);
    buf.extend_from_slice(&p.price.raw().to_be_bytes());
    buf.extend_from_slice(&p.quantity.raw().to_be_bytes());
    match &p.order_type {
        OrderType::Limit => buf.push(0),
        OrderType::Market => buf.push(1),
        OrderType::StopMarket { trigger } => {
            buf.push(2);
            buf.extend_from_slice(&trigger.raw().to_be_bytes());
        }
        OrderType::StopLimit { trigger, limit } => {
            buf.push(3);
            buf.extend_from_slice(&trigger.raw().to_be_bytes());
            buf.extend_from_slice(&limit.raw().to_be_bytes());
        }
    }
    buf.push(match p.time_in_force {
        TimeInForce::GTC => 0,
        TimeInForce::IOC => 1,
        TimeInForce::FOK => 2,
        TimeInForce::PostOnly => 3,
    });
    buf.push(p.reduce_only as u8);
    match p.client_order_id {
        Some(id) => {
            buf.push(1);
            buf.extend_from_slice(&id.to_be_bytes());
        }
        None => buf.push(0),
    }
}

fn legacy_canonical_bytes(action: &NativeAction) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    match action {
        NativeAction::PlaceOrder(p) => {
            buf.push(0);
            legacy_encode_place_order_params(&mut buf, p);
        }
        NativeAction::PlaceOrderBatch(orders) => {
            buf.push(25);
            buf.extend_from_slice(&(orders.len() as u32).to_be_bytes());
            for p in orders {
                legacy_encode_place_order_params(&mut buf, p);
            }
        }
        NativeAction::CancelOrder { order_id } => {
            buf.push(1);
            buf.extend_from_slice(&order_id.to_be_bytes());
        }
        NativeAction::CancelAllOrders { market_id } => {
            buf.push(2);
            match market_id {
                Some(id) => {
                    buf.push(1);
                    buf.extend_from_slice(&id.to_be_bytes());
                }
                None => buf.push(0),
            }
        }
        NativeAction::ModifyOrder {
            order_id,
            new_price,
            new_qty,
        } => {
            buf.push(3);
            buf.extend_from_slice(&order_id.to_be_bytes());
            match new_price {
                Some(p) => {
                    buf.push(1);
                    buf.extend_from_slice(&p.raw().to_be_bytes());
                }
                None => buf.push(0),
            }
            match new_qty {
                Some(q) => {
                    buf.push(1);
                    buf.extend_from_slice(&q.raw().to_be_bytes());
                }
                None => buf.push(0),
            }
        }
        NativeAction::TransferToPerp { amount } => {
            buf.push(4);
            buf.extend_from_slice(&amount.to_be_bytes::<32>());
        }
        NativeAction::TransferToSpot { amount } => {
            buf.push(5);
            buf.extend_from_slice(&amount.to_be_bytes::<32>());
        }
        NativeAction::Withdraw { amount, to } => {
            buf.push(6);
            buf.extend_from_slice(&amount.to_be_bytes::<32>());
            buf.extend_from_slice(to.as_slice());
        }
        NativeAction::Delegate { validator, amount } => {
            buf.push(7);
            buf.extend_from_slice(validator.as_slice());
            buf.extend_from_slice(&amount.to_be_bytes::<32>());
        }
        NativeAction::Undelegate { validator, amount } => {
            buf.push(8);
            buf.extend_from_slice(validator.as_slice());
            buf.extend_from_slice(&amount.to_be_bytes::<32>());
        }
        NativeAction::PermanentStake { amount } => {
            buf.push(9);
            buf.extend_from_slice(&amount.to_be_bytes::<32>());
        }
        NativeAction::ClaimRewards => {
            buf.push(10);
        }
        NativeAction::SubmitProposal(p) => {
            buf.push(11);
            buf.extend_from_slice(&(p.title.len() as u32).to_be_bytes());
            buf.extend_from_slice(p.title.as_bytes());
            buf.extend_from_slice(&(p.description.len() as u32).to_be_bytes());
            buf.extend_from_slice(p.description.as_bytes());
            match &p.action {
                ProposalAction::UpdateMarketParams { market_id, params } => {
                    buf.push(0);
                    buf.extend_from_slice(&market_id.to_be_bytes());
                    buf.extend_from_slice(&params.tick_size.raw().to_be_bytes());
                    buf.extend_from_slice(&params.lot_size.raw().to_be_bytes());
                    buf.extend_from_slice(&params.max_leverage.to_be_bytes());
                    buf.extend_from_slice(&params.maintenance_margin_bps.to_be_bytes());
                    buf.extend_from_slice(&params.max_funding_rate_bps.to_be_bytes());
                }
                ProposalAction::ListMarket(l) => {
                    buf.push(1);
                    buf.extend_from_slice(&(l.base_asset.len() as u32).to_be_bytes());
                    buf.extend_from_slice(l.base_asset.as_bytes());
                    buf.extend_from_slice(&(l.quote_asset.len() as u32).to_be_bytes());
                    buf.extend_from_slice(l.quote_asset.as_bytes());
                    buf.extend_from_slice(&l.tick_size.raw().to_be_bytes());
                    buf.extend_from_slice(&l.lot_size.raw().to_be_bytes());
                    buf.extend_from_slice(&l.max_leverage.to_be_bytes());
                    buf.extend_from_slice(&l.maintenance_margin_bps.to_be_bytes());
                }
                ProposalAction::DelistMarket { market_id } => {
                    buf.push(2);
                    buf.extend_from_slice(&market_id.to_be_bytes());
                }
                ProposalAction::ParameterChange { key, value } => {
                    buf.push(3);
                    buf.extend_from_slice(&(key.len() as u32).to_be_bytes());
                    buf.extend_from_slice(key.as_bytes());
                    buf.extend_from_slice(&(value.len() as u32).to_be_bytes());
                    buf.extend_from_slice(value.as_bytes());
                }
                ProposalAction::ValidatorRegistration { candidate } => {
                    buf.push(4);
                    buf.extend_from_slice(candidate.as_slice());
                }
            }
        }
        NativeAction::Vote {
            proposal_id,
            option,
        } => {
            buf.push(12);
            buf.extend_from_slice(&proposal_id.to_be_bytes());
            buf.push(match option {
                VoteOption::Yes => 0,
                VoteOption::No => 1,
                VoteOption::Abstain => 2,
            });
        }
        NativeAction::SubmitOraclePrices(s) => {
            buf.push(13);
            buf.extend_from_slice(&(s.prices.len() as u32).to_be_bytes());
            for (mid, price) in &s.prices {
                buf.extend_from_slice(&mid.to_be_bytes());
                buf.extend_from_slice(&price.raw().to_be_bytes());
            }
            buf.extend_from_slice(&s.timestamp.to_be_bytes());
        }
        NativeAction::RegisterValidator { pubkey, commission } => {
            buf.push(14);
            buf.extend_from_slice(&pubkey.0);
            buf.extend_from_slice(&commission.to_be_bytes());
        }
        NativeAction::UpdateCommission { new_rate } => {
            buf.push(15);
            buf.extend_from_slice(&new_rate.to_be_bytes());
        }
        NativeAction::JailVote { target } => {
            buf.push(16);
            buf.extend_from_slice(target.as_slice());
        }
        NativeAction::UnjailSelf => {
            buf.push(17);
        }
        NativeAction::RotateValidatorKey { new_pubkey } => {
            buf.push(18);
            buf.extend_from_slice(&new_pubkey.0);
        }
        NativeAction::UpdateMarketParams { market_id, params } => {
            buf.push(19);
            buf.extend_from_slice(&market_id.to_be_bytes());
            buf.extend_from_slice(&params.tick_size.raw().to_be_bytes());
            buf.extend_from_slice(&params.lot_size.raw().to_be_bytes());
            buf.extend_from_slice(&params.max_leverage.to_be_bytes());
            buf.extend_from_slice(&params.maintenance_margin_bps.to_be_bytes());
            buf.extend_from_slice(&params.max_funding_rate_bps.to_be_bytes());
        }
        NativeAction::ListMarket(l) => {
            buf.push(20);
            buf.extend_from_slice(&(l.base_asset.len() as u32).to_be_bytes());
            buf.extend_from_slice(l.base_asset.as_bytes());
            buf.extend_from_slice(&(l.quote_asset.len() as u32).to_be_bytes());
            buf.extend_from_slice(l.quote_asset.as_bytes());
            buf.extend_from_slice(&l.tick_size.raw().to_be_bytes());
            buf.extend_from_slice(&l.lot_size.raw().to_be_bytes());
            buf.extend_from_slice(&l.max_leverage.to_be_bytes());
            buf.extend_from_slice(&l.maintenance_margin_bps.to_be_bytes());
        }
        NativeAction::DelistMarket { market_id } => {
            buf.push(21);
            buf.extend_from_slice(&market_id.to_be_bytes());
        }
        NativeAction::TopUpSelfStake { amount } => {
            buf.push(22);
            buf.extend_from_slice(&amount.to_be_bytes::<32>());
        }
        NativeAction::CreateSession {
            session_pubkey,
            expiry,
            scope,
        } => {
            buf.push(23);
            buf.extend_from_slice(session_pubkey);
            buf.extend_from_slice(&expiry.to_be_bytes());
            buf.push(match scope {
                SessionScope::Trading => 0,
                SessionScope::TransfersOnly => 1,
                SessionScope::Full => 2,
            });
        }
        NativeAction::RevokeSession { session_pubkey } => {
            buf.push(24);
            buf.extend_from_slice(session_pubkey);
        }
    }
    buf
}

fn legacy_compute_action_hash(action: &SignedNativeAction) -> B256 {
    let mut data = legacy_canonical_bytes(&action.action);
    data.extend_from_slice(&action.nonce.to_be_bytes());
    // Domain-separated signature commitment. The leading tag byte keeps the two
    // variants' encodings disjoint so an EIP-712 action can never share a preimage
    // with a session action.
    match &action.signature {
        ActionSignature::Eip712(sig) => {
            data.push(0x00);
            data.push(sig.v);
            data.extend_from_slice(&sig.r);
            data.extend_from_slice(&sig.s);
        }
        ActionSignature::Session {
            session_pubkey,
            sig,
        } => {
            data.push(0x01);
            data.extend_from_slice(session_pubkey);
            data.extend_from_slice(&sig.0);
        }
    }
    alloy_primitives::keccak256(&data)
}

// Frozen pre-scratch trust-cache key: intentionally omits the content hash's
// signature tag and rejects sessions before encoding any action bytes.
fn legacy_verified_cache_key(action: &SignedNativeAction) -> Option<B256> {
    let ActionSignature::Eip712(sig) = &action.signature else {
        return None;
    };
    let mut data = legacy_canonical_bytes(&action.action);
    data.extend_from_slice(&action.nonce.to_be_bytes());
    data.push(sig.v);
    data.extend_from_slice(&sig.r);
    data.extend_from_slice(&sig.s);
    Some(alloy_primitives::keccak256(&data))
}

#[test]
fn trust_cache_scratch_matches_frozen_key_for_all_actions_and_signatures() {
    let mut scratch = vec![0xa5; 1024];
    for (i, action) in actions().into_iter().enumerate() {
        for session in [false, true] {
            for nonce in [0, 1, u64::MAX] {
                let action = signed(action.clone(), session, nonce);
                let expected = legacy_verified_cache_key(&action);
                assert_eq!(verified_cache_key(&action), expected, "wrapper {i}");
                assert_eq!(
                    verified_cache_key_with_scratch(&action, &mut scratch),
                    expected,
                    "scratch {i}"
                );
                if session {
                    assert!(scratch.is_empty());
                } else {
                    assert_ne!(expected.unwrap(), compute_action_hash(&action));
                }
                // Dirty bytes and Session None must not contaminate the next key.
                scratch.extend_from_slice(&[0x5a; 19]);
            }
        }
    }
}

#[test]
fn trust_cache_scratch_reuses_capacity_and_commits_each_signature_field() {
    let large = signed(
        NativeAction::PlaceOrderBatch((0..1024).map(order_params).collect()),
        false,
        u64::MAX,
    );
    let small = signed(NativeAction::ClaimRewards, false, 1);
    let session = signed(NativeAction::ClaimRewards, true, 1);
    let mut scratch = Vec::new();
    verified_cache_key_with_scratch(&large, &mut scratch).unwrap();
    let allocation = (scratch.as_ptr(), scratch.capacity());
    for action in [&small, &session, &large, &small] {
        assert_eq!(
            verified_cache_key_with_scratch(action, &mut scratch),
            legacy_verified_cache_key(action)
        );
        assert_eq!((scratch.as_ptr(), scratch.capacity()), allocation);
    }
    let original = verified_cache_key(&small).unwrap();
    for field in 0..4 {
        let mut changed = small.clone();
        let ActionSignature::Eip712(sig) = &mut changed.signature else { unreachable!() };
        match field {
            0 => sig.v ^= 1,
            1 => sig.r[0] ^= 1,
            2 => sig.s[31] ^= 1,
            _ => changed.nonce ^= 1,
        }
        let key = verified_cache_key_with_scratch(&changed, &mut scratch).unwrap();
        assert_eq!(Some(key), legacy_verified_cache_key(&changed));
        assert_ne!(key, original);
    }
}
