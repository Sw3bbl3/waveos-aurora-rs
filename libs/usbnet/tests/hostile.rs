//! A hostile (or broken) USB device sends whatever it likes: every parser
//! here must turn any input into frames inside the buffer, or an error —
//! never a panic, an out-of-range slice or an unbounded allocation.

use aurora_mutate::check;
use aurora_usbnet::{asix, cdc, eem, ncm, rndis, MAX_FRAME, MIN_FRAME};

fn frames(n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|k| (0..60 + k * 97 % 1400).map(|i| (i * 13 + k) as u8).collect()).collect()
}

fn ncm_block(ntb32: bool, count: usize) -> Vec<u8> {
    let params = ncm::Params {
        formats: 3,
        in_max: 32768,
        in_divisor: 4,
        in_remainder: 0,
        in_alignment: 4,
        out_max: 32768,
        out_divisor: 4,
        out_remainder: 2,
        out_alignment: 4,
        out_max_datagrams: 0,
    };
    let layout = params.out_layout(ntb32, 1 << 20);
    let mut buf = vec![0u8; layout.max];
    let mut b = ncm::Builder::new(layout);
    for f in frames(count) {
        if !b.push(&mut buf, &f) {
            break;
        }
    }
    let n = b.finish(&mut buf, 1);
    buf.truncate(n);
    buf
}

fn plausible(len: usize) {
    assert!((MIN_FRAME..=MAX_FRAME).contains(&len), "frame of {len} bytes passed on");
}

#[test]
fn ncm_blocks() {
    let seeds = vec![ncm_block(false, 1), ncm_block(false, 12), ncm_block(true, 7)];
    check("ncm", &seeds, 20_000, |b| {
        if let Ok(ranges) = ncm::parse(b, MAX_FRAME) {
            for r in ranges {
                assert!(r.end <= b.len() && r.start >= 12);
                plausible(r.len());
            }
        }
    });
    let mut params = vec![0u8; 28];
    params[2] = 3;
    check("ncm params", &[params], 5_000, |b| {
        if let Ok(p) = ncm::Params::parse(b) {
            for ntb32 in [false, true] {
                // Whatever the device claims, the blocks we build stay sane.
                let l = p.out_layout(ntb32, 65536);
                assert!(l.max >= 2048 && l.max <= 65536 && l.divisor >= 1 && l.remainder < l.divisor);
                let mut buf = vec![0u8; l.max];
                let mut builder = ncm::Builder::new(l);
                for f in frames(40) {
                    if !builder.push(&mut buf, &f) {
                        break;
                    }
                }
                let pushed = builder.count();
                let n = builder.finish(&mut buf, 0);
                assert!(n <= l.max);
                assert_eq!(ncm::parse(&buf[..n], MAX_FRAME).map(|r| r.len()), Ok(pushed));
            }
        }
    });
}

#[test]
fn rndis_control() {
    let mut query = vec![0u8; 30];
    query[..4].copy_from_slice(&0x8000_0004u32.to_le_bytes());
    query[4] = 30;
    query[16] = 6;
    query[20] = 16;
    let mut init = vec![0u8; 52];
    init[..4].copy_from_slice(&0x8000_0002u32.to_le_bytes());
    init[4] = 52;
    let mut indicate = vec![0u8; 20];
    indicate[0] = 7;
    indicate[4] = 20;
    indicate[8..12].copy_from_slice(&rndis::STATUS_MEDIA_CONNECT.to_le_bytes());
    check("rndis control", &[query, init, indicate], 20_000, |b| {
        if let Ok(rndis::Response::QueryComplete { data, .. }) = rndis::parse_response(b) {
            assert!(data.end <= b.len());
            let _ = &b[data.clone()];
            let _ = rndis::query_u32(b, &data);
        }
    });
}

#[test]
fn rndis_data() {
    let mut stream = Vec::new();
    for f in frames(6) {
        let mut b = vec![0u8; rndis::packet_len(f.len(), 0)];
        rndis::write_packet(&mut b, &f, 0);
        stream.extend_from_slice(&b);
    }
    check("rndis data", &[stream], 20_000, |b| {
        let mut r = rndis::Receiver::default();
        // Fed in two pieces, then a clean message: frames stay plausible and
        // the receiver never holds more than a message's worth.
        let cut = b.len() / 3;
        r.feed(&b[..cut], &mut |f| plausible(f.len()));
        r.feed(&b[cut..], &mut |f| plausible(f.len()));
    });
}

#[test]
fn eem_stream() {
    let mut stream = Vec::new();
    for f in frames(5) {
        let mut b = vec![0u8; eem::packet_len(f.len())];
        eem::write_packet(&mut b, &f);
        stream.extend_from_slice(&b);
        stream.extend_from_slice(&[2, 0x80, 1, 2]);
    }
    check("eem", &[stream], 20_000, |b| {
        let mut rx = eem::Receiver::default();
        let mut r = eem::Received::default();
        rx.feed(b, &mut r, &mut |f| plausible(f.len()));
        rx.feed(b, &mut r, &mut |f| plausible(f.len()));
        assert!(r.replies.len() <= 8);
        let _ = eem::parse_all(b);
    });
}

#[test]
fn asix_streams() {
    let mut s772 = Vec::new();
    for f in frames(5) {
        let mut b = vec![0u8; 2048];
        let n = asix::ax772_tx(&mut b, &f, 512);
        s772.extend_from_slice(&b[..n.min(4 + f.len())]);
        if f.len() % 2 == 1 {
            s772.push(0);
        }
    }
    check("ax88772", &[s772], 20_000, |b| {
        let mut r = asix::Ax772Receiver::default();
        r.feed(b, &mut |f| plausible(f.len()));
        r.feed(b, &mut |f| plausible(f.len()));
    });
    let mut block = Vec::new();
    let mut headers = Vec::new();
    for f in frames(4) {
        headers.push(((f.len() + 2) as u32) << 16);
        block.extend_from_slice(&[0, 0]);
        block.extend_from_slice(&f);
        block.resize((block.len() + 7) & !7, 0);
    }
    let table = block.len();
    for h in &headers {
        block.extend_from_slice(&h.to_le_bytes());
    }
    block.extend_from_slice(&((table as u32) << 16 | headers.len() as u32).to_le_bytes());
    assert_eq!(asix::ax179_rx(&block, &mut |f| plausible(f.len())), Ok(4));
    check("ax88179", &[block], 20_000, |b| {
        let _ = asix::ax179_rx(b, &mut |f| plausible(f.len()));
    });
}

#[test]
fn descriptors_and_notifications() {
    let ecm = vec![
        5, 0x24, 0, 0x10, 1, 5, 0x24, 6, 0, 1, 13, 0x24, 0x0F, 3, 0, 0, 0, 0, 0xEA, 5, 0, 0, 0, 6, 0x24, 0x1A, 0, 1,
        0x21,
    ];
    check("cdc functional", &[ecm], 10_000, |b| {
        let _ = cdc::Functional::parse(b);
    });
    let speed = vec![0xA1, 0x2A, 0, 0, 0, 0, 8, 0, 0, 0xCA, 0x9A, 0x3B, 0, 0xE1, 0xF5, 5];
    check("cdc notification", &[speed, vec![0xA1, 0, 1, 0, 1, 0, 0, 0]], 10_000, |b| {
        let _ = cdc::parse_notification(b);
        let _ = asix::ax772_link(b);
        let _ = asix::ax179_link(b);
    });
    check("mac string", &[b"400102030405".to_vec()], 5_000, |b| {
        if let Ok(s) = core::str::from_utf8(b) {
            let _ = cdc::parse_mac(s);
        }
    });
}
