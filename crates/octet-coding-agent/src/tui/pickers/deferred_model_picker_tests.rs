//! Tests for the deferred model picker: a catalog that becomes ready after the
//! picker has already opened must be applied to the live picker before later
//! input is handled.
//!
//! Separate from tests.rs because it is the only place in the picker area that
//! coordinates a background catalog with an already-running event stream, and
//! that ordering contract is easier to state on its own.

use super::*;
use crossterm::event::{KeyEvent, KeyModifiers};
use tokio_stream::wrappers::ReceiverStream;

#[tokio::test]
async fn deferred_model_picker_applies_ready_catalog_before_later_input() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let active = app.model.spec.id.clone();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    let (catalog, notes) = crate::app::bootstrap::model_catalog_for_readiness(
        app.config.offline,
        &crate::app::bootstrap::CatalogReadiness::Fleet,
    )
    .unwrap();
    let pending = tokio::spawn(async move { Ok((catalog, notes)) });
    while !pending.is_finished() {
        tokio::task::yield_now().await;
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();
    {
        let future = optional_model_picker_live(&mut shell, &mut input, &mut app, Some(pending));
        tokio::pin!(future);
        assert!(matches!(
            futures_util::poll!(future.as_mut()),
            std::task::Poll::Pending
        ));
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        assert!(future.await.unwrap().is_none());
    }
    assert!(app.readiness.is_fleet());
    assert_eq!(app.model.spec.id, active);
    assert!(!shell.has_panel());
}

#[tokio::test]
async fn deferred_model_picker_accepts_input_and_escape_before_inventory_finishes() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let initial = app.catalog.models().count();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    let (release, waiting) = tokio::sync::oneshot::channel::<()>();
    let pending = tokio::spawn(async move {
        waiting.await.unwrap();
        crate::app::bootstrap::model_catalog_for_readiness(
            true,
            &crate::app::bootstrap::CatalogReadiness::Fleet,
        )
    });
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(500),
        optional_model_picker_live(&mut shell, &mut input, &mut app, Some(pending)),
    )
    .await
    .expect("picker input must not wait for discovery")
    .unwrap()
    .is_none());
    assert!(!app.readiness.is_fleet());
    assert_eq!(app.catalog.models().count(), initial);
    assert!(!shell.has_panel());
    release.send(()).unwrap();
}

#[tokio::test]
async fn deferred_model_picker_failure_keeps_current_routes_and_stays_cancellable() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let initial = app.catalog.models().count();
    app.readiness = crate::app::bootstrap::CatalogReadiness::Routes(vec!["codex"]);
    let pending = tokio::spawn(async { anyhow::bail!("inventory unavailable") });
    while !pending.is_finished() {
        tokio::task::yield_now().await;
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();
    {
        let future = optional_model_picker_live(&mut shell, &mut input, &mut app, Some(pending));
        tokio::pin!(future);
        assert!(matches!(
            futures_util::poll!(future.as_mut()),
            std::task::Poll::Pending
        ));
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
        assert!(future.await.unwrap().is_none());
    }
    assert!(!app.readiness.is_fleet());
    assert_eq!(app.catalog.models().count(), initial);
    assert!(!shell.has_panel());
}
