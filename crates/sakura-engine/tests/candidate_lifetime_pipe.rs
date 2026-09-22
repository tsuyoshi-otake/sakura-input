//! Candidate ownership must end with its host pipe, even while WatchUi is healthy.

#[allow(dead_code)]
mod common;

use std::time::Duration;

use common::{char_key, named_key, session_for, Engine, PATIENT};
use sakura_ipc::Client;
use sakura_proto::{KeyCode, Request, Response, ScreenRect, UiState, PROTOCOL_VERSION};

fn handshake(client: &mut Client) {
    assert!(matches!(
        client.call(
            &Request::Hello {
                client_version: PROTOCOL_VERSION,
            },
            PATIENT,
        ),
        Ok(Response::Hello { server_version, .. }) if server_version == PROTOCOL_VERSION
    ));
}

fn watch(client: &mut Client, since: u64) -> UiState {
    match client.call(&Request::WatchUi { since }, Duration::from_secs(7)) {
        Ok(Response::Ui(state)) => state,
        other => panic!("expected UI snapshot, got {other:?}"),
    }
}

#[test]
fn disconnected_candidate_owner_clears_the_live_renderer_feed() {
    let mut engine = Engine::spawn_isolated();
    let mut host = engine.client();
    handshake(&mut host);
    let mut renderer = engine.client();
    handshake(&mut renderer);
    let session = session_for(&mut host, "candidate-owner.exe");
    for key in "kana"
        .chars()
        .map(char_key)
        .chain([named_key(KeyCode::Space)])
    {
        assert!(matches!(
            host.call(&Request::SendKey { session, key }, PATIENT),
            Ok(Response::Output(_))
        ));
    }
    assert!(matches!(
        host.call(
            &Request::SetUiPlacement {
                session,
                anchor: Some(ScreenRect {
                    left: 100,
                    top: 100,
                    right: 120,
                    bottom: 124,
                }),
                document: None,
                renderer_visible: true,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));
    let visible = watch(&mut renderer, 0);
    assert!(visible.candidates.is_some());
    assert!(visible.renderer_visible);

    // The renderer pipe stays connected; only the host owning the popup dies.
    drop(host);
    let hidden = watch(&mut renderer, visible.revision);
    assert!(hidden.candidates.is_none(), "dead host retained candidates");
    assert!(!hidden.renderer_visible);
    assert!(hidden.anchor.is_none());
    assert!(hidden.document.is_none());
    assert!(hidden.revision > visible.revision);

    // The server can accept a fresh host after disposing of the old session.
    let mut replacement = engine.client();
    handshake(&mut replacement);
    let _ = session_for(&mut replacement, "replacement-owner.exe");
    drop(replacement);
    drop(renderer);
    assert!(engine
        .cleanup()
        .expect("owned engine cleanup")
        .status
        .success());
}
