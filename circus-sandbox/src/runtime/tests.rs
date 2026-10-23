use super::{ResourceLimits, ResourceLimitsError};

#[test]
fn required_resource_counts_reject_zero() {
    let cases = [
        ((0, 1, 1, 1, 1), ResourceLimitsError::ZeroMemory),
        ((1, 0, 1, 1, 1), ResourceLimitsError::ZeroCpu),
        ((1, 1, 0, 1, 1), ResourceLimitsError::ZeroPids),
        ((1, 1, 1, 0, 1), ResourceLimitsError::ZeroWorkspace),
    ];
    for ((memory, cpu, pids, workspace, file), expected) in cases {
        assert_eq!(
            ResourceLimits::new(memory, cpu, pids, workspace, file),
            Err(expected)
        );
    }
}

#[test]
fn smallest_positive_request_is_valid() {
    assert!(ResourceLimits::new(1, 1, 1, 1, 1).is_ok());
}

#[test]
fn per_file_scalar_bounds_reject_zero_and_platform_maximum() {
    for max_file_bytes in [0, usize::MAX] {
        assert_eq!(
            ResourceLimits::new(1, 1, 1, 1, max_file_bytes),
            Err(ResourceLimitsError::InvalidMaxFileBytes { max_file_bytes })
        );
    }
}

#[test]
fn invalid_per_file_scalar_precedes_conflicting_workspace_relationship() {
    for max_file_bytes in [0, usize::MAX] {
        assert_eq!(
            ResourceLimits::new(1, 1, 1, 2, max_file_bytes),
            Err(ResourceLimitsError::InvalidMaxFileBytes { max_file_bytes })
        );
    }
}

#[test]
fn memory_accepts_signed_maximum_and_rejects_larger_values() {
    let maximum = i64::MAX as u64;
    assert!(ResourceLimits::new(maximum, 1, 1, 1, 1).is_ok());
    for memory_bytes in [maximum + 1, u64::MAX] {
        assert_eq!(
            ResourceLimits::new(memory_bytes, 1, 1, 1, 1),
            Err(ResourceLimitsError::MemoryOutOfRange { memory_bytes })
        );
    }
}

#[test]
fn workspace_accepts_signed_maximum_and_rejects_larger_values() {
    let maximum = i64::MAX as u64;
    assert!(ResourceLimits::new(maximum, 1, 1, maximum, 1).is_ok());
    for workspace_bytes in [maximum + 1, u64::MAX] {
        assert_eq!(
            ResourceLimits::new(maximum, 1, 1, workspace_bytes, 1),
            Err(ResourceLimitsError::WorkspaceOutOfRange { workspace_bytes })
        );
    }
}

#[test]
fn workspace_may_equal_but_not_exceed_memory() {
    assert!(ResourceLimits::new(8, 1, 1, 7, 1).is_ok());
    assert!(ResourceLimits::new(8, 1, 1, 8, 1).is_ok());
    assert_eq!(
        ResourceLimits::new(8, 1, 1, 9, 1),
        Err(ResourceLimitsError::WorkspaceExceedsMemory {
            workspace_bytes: 9,
            memory_bytes: 8,
        })
    );
}

#[test]
fn per_file_limit_may_equal_but_not_exceed_aggregate_workspace() {
    for max_file_bytes in [1, 15, 16] {
        assert!(ResourceLimits::new(32, 1, 1, 16, max_file_bytes).is_ok());
    }
    assert_eq!(
        ResourceLimits::new(32, 1, 1, 16, 17),
        Err(ResourceLimitsError::MaxFileExceedsWorkspace {
            max_file_bytes: 17,
            workspace_bytes: 16,
        })
    );
}

#[test]
fn all_linked_limits_may_be_exactly_equal() {
    assert!(ResourceLimits::new(1, 1, 1, 1, 1).is_ok());
    assert!(ResourceLimits::new(4096, 1, 1, 4096, 4096).is_ok());
}

#[test]
fn platform_sized_per_file_limit_is_compared_without_truncation() {
    let workspace_bytes = i64::MAX as u64;
    let max_file_bytes = usize::MAX - 1;
    let result = ResourceLimits::new(workspace_bytes, 1, 1, workspace_bytes, max_file_bytes);
    if max_file_bytes as u128 <= u128::from(workspace_bytes) {
        assert!(result.is_ok());
    } else {
        assert_eq!(
            result,
            Err(ResourceLimitsError::MaxFileExceedsWorkspace {
                max_file_bytes,
                workspace_bytes,
            })
        );
    }
}

#[test]
fn cpu_and_process_counts_have_no_backend_specific_maximum() {
    assert!(ResourceLimits::new(1, u32::MAX, u32::MAX, 1, 1).is_ok());
}
