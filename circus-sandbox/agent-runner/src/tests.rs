mod docker;

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::io::BufReader;

use crate::events::{EVENT_QUEUE, Events, MAX_EVENT_FRAME, WireEvent};
use crate::{ModelConfig, TaskError, TaskEvent, TaskRequest};

fn request() -> TaskRequest {
    TaskRequest {
        prompt: "Fix the test".into(),
        input_files: BTreeMap::from([("src/main.py".into(), b"pass\n".to_vec())]),
        output_paths: vec!["src/main.py".into()],
        timeout: Duration::from_secs(30),
    }
}

#[test]
fn task_paths_budgets_and_duplicates_are_rejected_before_allocation() {
    request().validate(1024).unwrap();
    for invalid in [
        "",
        "/etc/passwd",
        "../secret",
        "a/../b",
        "a//b",
        "a/./b",
        "C:secret",
        "a\\b",
        "a\0b",
    ] {
        let mut task = request();
        task.output_paths = vec![invalid.into()];
        assert!(
            matches!(task.validate(1024), Err(TaskError::Invalid(_))),
            "{invalid:?}"
        );
        task = request();
        task.input_files.insert(invalid.into(), vec![1]);
        assert!(task.validate(1024).is_err());
    }
    let mut task = request();
    task.output_paths.push(task.output_paths[0].clone());
    assert!(task.validate(1024).is_err());
    task = request();
    task.input_files.insert("large".into(), vec![0; 1025]);
    assert!(task.validate(1024).is_err());
    task = request();
    task.timeout = Duration::ZERO;
    assert!(task.validate(1024).is_err());
}

#[test]
fn model_configuration_is_explicit_and_debug_does_not_expose_credentials() {
    let model = ModelConfig::new(
        "https://gateway.example/v1".into(),
        "model".into(),
        "private-test-key".into(),
    )
    .unwrap();
    assert!(!format!("{model:?}").contains("private-test-key"));
    for url in [
        "http://gateway.example/v1",
        "https://user:secret@gateway.example/v1",
        "https://gateway.example/v1?key=secret",
        "https://gateway.example/v1#secret",
    ] {
        assert!(ModelConfig::new(url.into(), "model".into(), "key".into()).is_err());
    }
}

#[tokio::test]
async fn event_queue_and_frames_have_hard_limits() {
    let (sender, _receiver) = tokio::sync::mpsc::channel(EVENT_QUEUE);
    let mut events = Events::new(sender);
    for _ in 0..EVENT_QUEUE {
        events
            .send(TaskEvent::TextDelta {
                text: "hello".into(),
            })
            .unwrap();
    }
    assert!(matches!(
        events.send(TaskEvent::TextDelta {
            text: "overflow".into()
        }),
        Err(TaskError::EventBackpressure)
    ));
    let oversized = vec![b'x'; MAX_EVENT_FRAME + 1];
    assert!(
        crate::process::read_frame(&mut BufReader::new(oversized.as_slice()))
            .await
            .is_err()
    );
    assert!(
        crate::process::read_frame(&mut BufReader::new(&b"{}"[..]))
            .await
            .is_err()
    );
    for invalid in [
        b"{\"kind\":\"text\",\"text\":\"a\",\"text\":\"b\"}".as_slice(),
        b"{\"kind\":\"text\",\"text\":NaN}",
        b"\xff",
    ] {
        assert!(sonic_rs::from_slice::<WireEvent>(invalid).is_err());
    }
}
