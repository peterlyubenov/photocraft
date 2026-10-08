use super::*;
use crate::ai_cmds::tests::workflow;
use photocraft_codecs::{ChannelLayout, Image};
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Size};
#[test]
fn cancelled_late_results_and_arrival_after_own_acceptance() {
    let doc = Document::new("test", Size::new(32, 32), ColorMode::Rgb, SampleType::U8);
    let request = Request { prompt: "test".into(), width: 8, height: 8, ..Default::default() };
    let prepared = super::super::images::prepare(&doc, 1, None, &request, &workflow(), 0).unwrap();
    let make = || Candidate {
        id: 1,
        placement: prepared.placement.clone(),
        image: Image::from_u8(8, 8, ChannelLayout::Rgb, vec![128; 192]).unwrap(),
        metadata: json!({}),
    };
    let (sender, receiver) = mpsc::channel();
    let flag = Arc::new(AtomicBool::new(false));
    let mut queue = Generation::default();
    queue.worker = Some(Worker { receiver, cancel: flag.clone(), cancel_pending: Arc::new(AtomicBool::new(false)) });
    queue.request_document = Some(doc.id);
    queue.expected_revision = Some(1);
    queue.status.running = true;
    queue.accepted_revision(doc.id, 3);
    queue.cancel_pending();
    assert!(queue.worker.as_ref().unwrap().cancel_pending.load(Ordering::Relaxed));
    assert!(!flag.load(Ordering::Relaxed));
    sender.send(Event::Candidate(Box::new(make()))).unwrap();
    queue.tick();
    assert_eq!(queue.candidates[0].placement.revision, 3);
    queue.cancel();
    sender.send(Event::Candidate(Box::new(make()))).unwrap();
    sender.send(Event::Finished(Err(AiError::Cancelled))).unwrap();
    queue.tick();
    assert_eq!(queue.candidates.len(), 1);
    assert!(!queue.status.running);
    assert!(queue.status.message.contains("cancelled"));
}
