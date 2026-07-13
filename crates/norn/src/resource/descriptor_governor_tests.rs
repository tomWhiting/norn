use std::sync::Arc;
use std::time::Duration;

use super::DescriptorGovernor;
use crate::resource::{DescriptorLimits, DescriptorOpenCount, DescriptorSnapshot};

#[test]
fn explicit_unlimited_soft_limit_still_builds_an_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let snapshot = DescriptorSnapshot {
        limits: Some(DescriptorLimits {
            soft: None,
            hard: None,
        }),
        limits_error: None,
        open: Some(DescriptorOpenCount {
            count: 4,
            source: "test",
            includes_observer: false,
        }),
        open_error: None,
    };
    let governor = DescriptorGovernor::from_snapshot(&snapshot)?;
    assert!(governor.capacity > 0);
    Ok(())
}

#[tokio::test]
async fn weighted_waiter_enters_only_after_capacity_returns()
-> Result<(), Box<dyn std::error::Error>> {
    let governor = Arc::new(DescriptorGovernor::with_capacity(3));
    let held = governor.acquire(2).await?;
    let waiter = {
        let governor = Arc::clone(&governor);
        tokio::spawn(async move { governor.acquire(2).await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!waiter.is_finished());
    drop(held);
    let admitted = waiter.await??;
    assert_eq!(governor.available(), 1);
    drop(admitted);
    assert_eq!(governor.available(), 3);
    Ok(())
}

#[tokio::test]
async fn cancelled_waiter_leaks_no_capacity() -> Result<(), Box<dyn std::error::Error>> {
    let governor = Arc::new(DescriptorGovernor::with_capacity(2));
    let held = governor.acquire(2).await?;
    let waiter = {
        let governor = Arc::clone(&governor);
        tokio::spawn(async move { governor.acquire(1).await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    waiter.abort();
    let _ = waiter.await;
    drop(held);
    assert_eq!(governor.available(), 2);
    Ok(())
}

#[tokio::test]
async fn split_permits_release_independent_lifetimes() -> Result<(), Box<dyn std::error::Error>> {
    let governor = DescriptorGovernor::with_capacity(3);
    let mut permit = governor.acquire(3).await?;
    let split = permit
        .split(2)
        .ok_or_else(|| std::io::Error::other("failed to split owned permit"))?;
    drop(permit);
    assert_eq!(governor.available(), 1);
    drop(split);
    assert_eq!(governor.available(), 3);
    Ok(())
}

#[tokio::test]
async fn impossible_weight_fails_before_waiting() -> Result<(), Box<dyn std::error::Error>> {
    let governor = DescriptorGovernor::with_capacity(2);
    let error = governor
        .acquire(3)
        .await
        .err()
        .ok_or_else(|| std::io::Error::other("oversized admission unexpectedly succeeded"))?;
    assert!(error.to_string().contains("exceeds"));
    assert_eq!(governor.available(), 2);
    Ok(())
}

#[tokio::test]
async fn fail_fast_admission_recovers_after_capacity_returns()
-> Result<(), Box<dyn std::error::Error>> {
    let governor = DescriptorGovernor::with_capacity(2);
    let held = governor.acquire(2).await?;
    let error = governor
        .try_acquire(1)
        .err()
        .ok_or_else(|| std::io::Error::other("busy admission unexpectedly succeeded"))?;
    assert!(error.to_string().contains("capacity is busy"));
    drop(held);
    let admitted = governor.try_acquire(1)?;
    assert_eq!(governor.available(), 1);
    drop(admitted);
    assert_eq!(governor.available(), 2);
    Ok(())
}
