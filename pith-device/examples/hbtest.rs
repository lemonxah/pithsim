//! Quick bench check: open the handbrake over HID and run the @CAP/? dance,
//! printing each step — for diagnosing "Connect failed" states.
fn main() {
    let mut hb = pith_device::Handbrake::default();
    println!(
        "present: {}",
        pith_device::device_present(pith_device::PITH_VID, pith_device::PID_HANDBRAKE)
    );
    let t = std::time::Instant::now();
    let ok = hb.connect();
    println!("connect: {ok} ({:?})", t.elapsed());
    if !ok {
        return;
    }
    for i in 0..3 {
        let t = std::time::Instant::now();
        let caps = hb.capabilities();
        println!("caps[{i}]: {caps:?} ({:?})", t.elapsed());
        let t = std::time::Instant::now();
        let st = hb.status();
        println!("status[{i}]: {st:?} ({:?})", t.elapsed());
    }
}
