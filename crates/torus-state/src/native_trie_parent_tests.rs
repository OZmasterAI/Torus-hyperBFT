// Frozen 080c4fa stage7 propagation, independent of production parent collection.
use super::*;

fn legacy_propagate(
    changed: &mut BTreeMap<(usize, usize), B256>,
    mut read_sibling: impl FnMut(usize, usize) -> Result<B256, StateError>,
) -> Result<(), StateError> {
    for level in (1..=TREE_DEPTH).rev() {
        let level_indices: Vec<usize> = changed
            .range((level, 0)..(level + 1, 0))
            .map(|(&(_, i), _)| i)
            .collect();
        let parents: BTreeSet<usize> = level_indices.iter().map(|i| i / 2).collect();
        for p in parents {
            let left = match changed.get(&(level, 2 * p)) {
                Some(v) => *v,
                None => read_sibling(level, 2 * p)?,
            };
            let right = match changed.get(&(level, 2 * p + 1)) {
                Some(v) => *v,
                None => read_sibling(level, 2 * p + 1)?,
            };
            changed.insert((level - 1, p), hash_pair(&left, &right));
        }
    }
    Ok(())
}

fn leaves(indices: impl IntoIterator<Item = usize>) -> BTreeMap<(usize, usize), B256> {
    indices
        .into_iter()
        .map(|index| ((TREE_DEPTH, index), keccak256((index as u64).to_be_bytes())))
        .collect()
}

fn sibling_value(level: usize, index: usize) -> B256 {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&(level as u64).to_be_bytes());
    bytes[8..].copy_from_slice(&(index as u64).to_be_bytes());
    keccak256(bytes)
}

#[test]
fn parent_scratch_matches_legacy_propagation_and_sibling_read_order() {
    let cases = [
        leaves([]),
        leaves([0]),
        leaves([65535]),
        leaves([0, 1]),
        leaves([0, 1, 2, 3, 32767, 32768, 65534, 65535]),
        leaves(0..256),
        leaves((0..65536).step_by(257)),
        leaves((0..65536).step_by(1024).flat_map(|i| [i, i + 1, i + 7])),
    ];
    for initial in cases {
        let mut expected = initial.clone();
        let mut old_reads = Vec::new();
        legacy_propagate(&mut expected, |level, index| {
            old_reads.push((level, index));
            Ok(sibling_value(level, index))
        })
        .unwrap();
        let mut actual = initial.clone();
        let mut new_reads = Vec::new();
        propagate_changed_nodes(&mut actual, |level, index| {
            new_reads.push((level, index));
            Ok(sibling_value(level, index))
        })
        .unwrap();
        assert_eq!(new_reads, old_reads);
        assert_eq!(actual, expected);

        // With default siblings, the unchanged full-tree builder is also an
        // independent oracle for the final root (including the empty input).
        let defaults = default_nodes();
        let full_leaves = initial.iter().map(|(&(_, i), &h)| (i as u16, h)).collect();
        let (full_root, _) = build_tree(&full_leaves, &defaults);
        let mut actual = initial;
        propagate_changed_nodes(&mut actual, |level, _| Ok(defaults[level])).unwrap();
        assert_eq!(
            actual.get(&(0, 0)).copied().unwrap_or(defaults[0]),
            full_root
        );
    }
}

#[test]
fn parent_scratch_preserves_first_error_and_partial_changed_map() {
    // Missing left and right siblings at leaf, middle and top levels. Fail at
    // every read boundary, checking both the exact read prefix and the parent
    // inserts completed before that error; no later sibling may be read.
    for initial in [
        leaves([0]),
        leaves([65535]),
        leaves([0, 1, 2, 32768, 65535]),
    ] {
        let mut reads = Vec::new();
        legacy_propagate(&mut initial.clone(), |level, index| {
            reads.push((level, index));
            Ok(sibling_value(level, index))
        })
        .unwrap();
        for fail_at in 0..reads.len() {
            let mut expected = initial.clone();
            let mut old_reads = Vec::new();
            let old = legacy_propagate(&mut expected, |level, index| {
                old_reads.push((level, index));
                if old_reads.len() == fail_at + 1 {
                    Err(StateError::InvalidData(format!("sibling {level}:{index}")))
                } else {
                    Ok(sibling_value(level, index))
                }
            })
            .unwrap_err();
            let mut actual = initial.clone();
            let mut new_reads = Vec::new();
            let new = propagate_changed_nodes(&mut actual, |level, index| {
                new_reads.push((level, index));
                if new_reads.len() == fail_at + 1 {
                    Err(StateError::InvalidData(format!("sibling {level}:{index}")))
                } else {
                    Ok(sibling_value(level, index))
                }
            })
            .unwrap_err();
            assert_eq!(new.to_string(), old.to_string());
            assert_eq!(new_reads, reads[..=fail_at]);
            assert_eq!(new_reads, old_reads);
            assert_eq!(actual, expected);
        }
    }
}
