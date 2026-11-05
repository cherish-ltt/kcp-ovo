use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::error::IncompleteDataType;
use kcp_ovo::helper::{KcpPacket, KcpPacketHeader, PacketBuilder};
use kcp_ovo::{KcpCmd, KcpConfig, KcpError, Segment};

fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("in-memory output must complete without a runtime"),
    }
}

#[test]
fn packet_and_segment_use_standard_owned_and_borrowed_bytes() {
    let payload = b"payload";
    let header = KcpPacketHeader::new(7, KcpCmd::Push, 0, 128, 1000, 0, 0, 7);
    let encoded_header: Vec<u8> = header.to_bytes().unwrap();
    let (decoded, remaining): (KcpPacketHeader, &[u8]) =
        KcpPacketHeader::from_bytes(&mut encoded_header.as_slice()).unwrap();
    assert_eq!(decoded, header);
    assert!(remaining.is_empty());

    let packet = KcpPacket::new_with_data(header, payload.to_vec());
    let mut encoded: Vec<u8> = packet.to_bytes().unwrap();
    encoded.extend_from_slice(b"tail");
    let mut input: &[u8] = &encoded;
    let parsed = KcpPacket::from_bytes(&mut input).unwrap();
    assert_eq!(input, b"payloadtail");
    let data: Vec<u8> = parsed.data;
    encoded.fill(0);
    assert_eq!(data, payload);

    let copied = KcpPacket::from_header_and_data(header, b"payloadtail").unwrap();
    assert_eq!(copied.data, payload);
    let segment = Segment::new_with_header_and_data(header, payload).unwrap();
    let borrowed: &[u8] = segment.get_data();
    assert_eq!(borrowed, payload);
    let built = PacketBuilder::new(7).command(KcpCmd::Push).data(data).build();
    assert_eq!(built.data, payload);
    assert!(KcpPacket::new_without_data(header).data.is_empty());
}

#[test]
fn core_transfers_owned_vectors_without_a_runtime() {
    for config in [KcpConfig::fast_mode(), KcpConfig::stream_mode()] {
        let mut sender = Kcp::new(7, config.clone()).unwrap();
        let mut receiver = Kcp::new(7, config).unwrap();
        let outgoing = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let output = outgoing.clone();
        sender.set_output(move |packet: Vec<u8>| {
            let size = packet.len();
            output.lock().unwrap().push(packet);
            async move { Ok(size) }
        });
        let acknowledgements = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let output = acknowledgements.clone();
        receiver.set_output(move |packet: Vec<u8>| {
            let size = packet.len();
            output.lock().unwrap().push(packet);
            async move { Ok(size) }
        });
        let payload: Vec<u8> = (0..4096).map(|index| (index % 251) as u8).collect();
        assert_eq!(sender.send(&payload).unwrap(), payload.len());
        let mut delivered = Vec::new();
        for now in 0..1000 {
            ready(sender.update(now)).unwrap();
            for packet in outgoing.lock().unwrap().drain(..) {
                receiver.input(&packet).unwrap();
            }
            match receiver.recv() {
                Ok(data) => {
                    let data: Vec<u8> = data;
                    delivered.extend(data);
                },
                Err(KcpError::QueueEmpty) => {},
                Err(KcpError::IncompleteData(IncompleteDataType::PayloadWaitForFrg)) => {},
                Err(error) => panic!("receive failed: {error}"),
            }
            ready(receiver.update(now)).unwrap();
            for packet in acknowledgements.lock().unwrap().drain(..) {
                sender.input(&packet).unwrap();
            }
            if delivered.len() == payload.len() && sender.waitsnd() == 0 {
                break;
            }
        }
        assert_eq!(delivered, payload);
        assert_eq!(sender.waitsnd(), 0);
        assert_eq!(receiver.recv().unwrap_err(), KcpError::QueueEmpty);
    }
}
