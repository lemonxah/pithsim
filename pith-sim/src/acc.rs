//! Assetto Corsa Competizione "Broadcasting" UDP protocol (port 9000).
//!
//! Unlike the passive decoders, ACC is an **active, stateful** client: we send a
//! REGISTER datagram to the game and it streams session + per-car updates back.
//! The protocol is a spectator/timing feed — it carries gear, km/h, position,
//! lap/sector times, delta and normalized track position, but **no engine RPM,
//! pedals, tyre temps or fuel** (those live only in ACC's shared memory, which is
//! what the SimHub plugin reads). So ACC-over-UDP drives the dash + track map but
//! not the shift lights.
//!
//! Wire format: little-endian; strings = u16 length + UTF-8; protocol version 4.

const PROTOCOL_VERSION: u8 = 4;

// Outbound message type bytes.
pub const REGISTER: u8 = 1;
pub const UNREGISTER: u8 = 9;
pub const REQUEST_ENTRY_LIST: u8 = 10;
pub const REQUEST_TRACK_DATA: u8 = 11;

// Inbound message type bytes.
const REGISTRATION_RESULT: u8 = 1;
const REALTIME_UPDATE: u8 = 2;
const REALTIME_CAR_UPDATE: u8 = 3;
const ENTRY_LIST_CAR: u8 = 6;
const TRACK_DATA: u8 = 5;

const INVALID_LAP: i32 = i32::MAX; // sentinel for "no time"

/// Build the REGISTER datagram.
pub fn encode_register(display_name: &str, conn_pw: &str, interval_ms: i32, cmd_pw: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(32);
    b.push(REGISTER);
    b.push(PROTOCOL_VERSION);
    put_str(&mut b, display_name);
    put_str(&mut b, conn_pw);
    b.extend_from_slice(&interval_ms.to_le_bytes());
    put_str(&mut b, cmd_pw);
    b
}

/// Build a `[type][connectionId i32]` request (entry list / track data / unregister).
pub fn encode_request(msg_type: u8, connection_id: i32) -> Vec<u8> {
    let mut b = Vec::with_capacity(5);
    b.push(msg_type);
    b.extend_from_slice(&connection_id.to_le_bytes());
    b
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    b.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    b.extend_from_slice(bytes);
}

/// A decoded inbound message (only the ones we act on).
pub enum AccMsg {
    /// Registration accepted: carries the connection id to echo in later requests.
    Registered { connection_id: i32 },
    /// Registration rejected.
    RegisterFailed,
    /// Session update — tells us which car is focused (the player/spectated car).
    Realtime { focused_car_index: i32 },
    /// Per-car update for `car_index`.
    Car(CarUpdate),
    /// One entry-list car: index + display name (driver surname, else team) +
    /// race number — feeds the relatives/standings widget's labels.
    EntryCar { car_index: i32, name: String, race_number: i32 },
    /// Track info: name + length in meters (needed to turn spline positions
    /// into on-track gaps).
    Track { name: String, meters: i32 },
    /// A message type we don't act on.
    Other,
}

/// The fields we extract from a RealtimeCarUpdate.
#[derive(Clone)]
pub struct CarUpdate {
    pub car_index: i32,
    pub gear: i32, // -1 = R, 0 = N, 1.. forward
    pub kmh: i32,
    pub position: i32,
    pub laps: i32,
    pub delta_ms: i32,
    pub spline: f32, // 0..1 normalized lap distance
    pub world_x: f32,
    pub world_y: f32,
    pub best_ms: i32,
    pub last_ms: i32,
    pub cur_ms: i32,
    pub sectors: [i32; 3],
    /// CarLocationEnum: 0 none, 1 track, 2 pitlane, 3 pit entry, 4 pit exit.
    pub location: u8,
}

impl CarUpdate {
    pub fn in_pits(&self) -> bool {
        matches!(self.location, 2..=4)
    }
}

/// Parse one inbound datagram.
pub fn parse(b: &[u8]) -> Option<AccMsg> {
    let mut c = Cur { b, i: 0 };
    match c.u8()? {
        REGISTRATION_RESULT => {
            let connection_id = c.i32()?;
            let success = c.u8()? > 0;
            Some(if success {
                AccMsg::Registered { connection_id }
            } else {
                AccMsg::RegisterFailed
            })
        }
        REALTIME_UPDATE => {
            c.skip(2)?; // eventIndex
            c.skip(2)?; // sessionIndex
            c.skip(1)?; // sessionType
            c.skip(1)?; // phase
            c.skip(4)?; // sessionTime f32
            c.skip(4)?; // sessionEndTime f32
            let focused_car_index = c.i32()?;
            Some(AccMsg::Realtime { focused_car_index })
        }
        REALTIME_CAR_UPDATE => {
            let car_index = c.u16()? as i32;
            c.skip(2)?; // driverIndex
            c.skip(1)?; // driverCount
            // Gear bias: LIVE ACC (verified in-game 2026-07: with -2 every gear
            // displayed one LOW — 1st showed N, N showed R) sends the shm-style
            // encoding raw 0=R, 1=N, 2=1st → decode raw-1. NOTE the official
            // ksBroadcastingNetwork client source says `ReadByte() - 2` ("-2
            // makes the R -1, N 0"), which contradicts what the shipping game
            // actually emits — trust the live packet, not the SDK mirror. If
            // gear ever reads one HIGH here, capture the raw byte before
            // changing this again.
            let gear = c.u8()? as i32 - 1;
            let world_x = c.f32()?;
            let world_y = c.f32()?;
            c.skip(4)?; // yaw f32
            let location = c.u8()?; // carLocation (pit detection for relatives)
            let kmh = c.u16()? as i32;
            let position = c.u16()? as i32;
            c.skip(2)?; // cupPosition
            c.skip(2)?; // trackPosition
            let spline = c.f32()?;
            let laps = c.u16()? as i32;
            let delta_ms = c.i32()?;
            let (best_ms, _) = c.lap()?;
            let (last_ms, _) = c.lap()?;
            let (cur_ms, sectors) = c.lap()?;
            Some(AccMsg::Car(CarUpdate {
                car_index,
                gear,
                kmh,
                position,
                laps,
                delta_ms,
                spline,
                world_x,
                world_y,
                best_ms,
                last_ms,
                cur_ms,
                sectors,
                location,
            }))
        }
        ENTRY_LIST_CAR => {
            // carIndex u16, carModelType u8, teamName str, raceNumber i32,
            // cupCategory u8, currentDriverIndex u8, nationality u16,
            // driverCount u8, then per driver: first/last/short str + cat u8 +
            // nationality u16. Label = first driver's last name, else team.
            let car_index = c.u16()? as i32;
            c.skip(1)?; // carModelType
            let team = c.str()?;
            let race_number = c.i32()?;
            c.skip(1)?; // cupCategory
            c.skip(1)?; // currentDriverIndex
            c.skip(2)?; // nationality
            let drivers = c.u8()?;
            let mut name = String::new();
            if drivers > 0 {
                let _first = c.str()?;
                let last = c.str()?;
                let _short = c.str()?;
                name = last;
            }
            if name.trim().is_empty() {
                name = team;
            }
            Some(AccMsg::EntryCar { car_index, name: name.trim().to_string(), race_number })
        }
        TRACK_DATA => {
            c.skip(4)?; // connectionId
            let name = c.str()?;
            c.skip(4)?; // trackId
            let meters = c.i32()?;
            Some(AccMsg::Track { name, meters })
        }
        _ => Some(AccMsg::Other),
    }
}

/// Bounds-checked little-endian cursor.
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i + n)?;
        self.i += n;
        Some(s)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn i32(&mut self) -> Option<i32> {
        let s = self.take(4)?;
        Some(i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn f32(&mut self) -> Option<f32> {
        let s = self.take(4)?;
        Some(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    /// Protocol string: u16 length + UTF-8 bytes.
    fn str(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        let s = self.take(n)?;
        Some(String::from_utf8_lossy(s).into_owned())
    }
    /// Read a Lap struct, returning (laptimeMs with sentinel→0, up to 3 sector ms).
    fn lap(&mut self) -> Option<(i32, [i32; 3])> {
        let ms = self.i32()?;
        let ms = if ms == INVALID_LAP { 0 } else { ms };
        self.skip(2)?; // carIndex
        self.skip(2)?; // driverIndex
        let n = self.u8()? as usize;
        let mut splits = [0i32; 3];
        for k in 0..n {
            let s = self.i32()?;
            if let Some(slot) = splits.get_mut(k) {
                *slot = if s == INVALID_LAP { 0 } else { s };
            }
        }
        self.skip(4)?; // isInvalid, isValidForBest, isOutLap, isInLap
        Some((ms, splits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_roundtrip_shape() {
        let r = encode_register("Pith", "asd", 250, "");
        assert_eq!(r[0], REGISTER);
        assert_eq!(r[1], PROTOCOL_VERSION);
        // "Pith" length prefix
        assert_eq!(u16::from_le_bytes([r[2], r[3]]), 4);
        assert_eq!(&r[4..8], b"Pith");
    }

    fn put_lap(b: &mut Vec<u8>, ms: i32) {
        b.extend_from_slice(&ms.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // carIndex
        b.extend_from_slice(&0u16.to_le_bytes()); // driverIndex
        b.push(3); // splitCount
        for _ in 0..3 {
            b.extend_from_slice(&0i32.to_le_bytes());
        }
        b.extend_from_slice(&[0, 0, 0, 0]); // flags
    }

    #[test]
    fn parse_car_update() {
        let mut b = vec![REALTIME_CAR_UPDATE];
        b.extend_from_slice(&7u16.to_le_bytes()); // carIndex
        b.extend_from_slice(&0u16.to_le_bytes()); // driverIndex
        b.push(1); // driverCount
        b.push(5); // gear byte → 5-1 = 4 (live game sends shm-style gear+1)
        b.extend_from_slice(&10.0f32.to_le_bytes()); // worldX
        b.extend_from_slice(&20.0f32.to_le_bytes()); // worldY
        b.extend_from_slice(&0.0f32.to_le_bytes()); // yaw
        b.push(1); // carLocation
        b.extend_from_slice(&210u16.to_le_bytes()); // kmh
        b.extend_from_slice(&3u16.to_le_bytes()); // position
        b.extend_from_slice(&3u16.to_le_bytes()); // cupPosition
        b.extend_from_slice(&3u16.to_le_bytes()); // trackPosition
        b.extend_from_slice(&0.5f32.to_le_bytes()); // spline
        b.extend_from_slice(&4u16.to_le_bytes()); // laps
        b.extend_from_slice(&(-1234i32).to_le_bytes()); // delta
        put_lap(&mut b, i32::MAX); // best (none)
        put_lap(&mut b, 83450); // last
        put_lap(&mut b, 12345); // current
        match parse(&b).unwrap() {
            AccMsg::Car(u) => {
                assert_eq!(u.car_index, 7);
                assert_eq!(u.gear, 4);
                assert_eq!(u.kmh, 210);
                assert_eq!(u.position, 3);
                assert_eq!(u.laps, 4);
                assert_eq!(u.delta_ms, -1234);
                assert_eq!(u.best_ms, 0); // sentinel
                assert_eq!(u.last_ms, 83450);
                assert_eq!(u.cur_ms, 12345);
            }
            _ => panic!("expected car update"),
        }
    }

    #[test]
    fn parse_realtime_focused() {
        let mut b = vec![REALTIME_UPDATE];
        b.extend_from_slice(&[0u8; 2 + 2 + 1 + 1 + 4 + 4]); // up to focusedCarIndex
        b.extend_from_slice(&9i32.to_le_bytes());
        match parse(&b).unwrap() {
            AccMsg::Realtime { focused_car_index } => assert_eq!(focused_car_index, 9),
            _ => panic!("expected realtime"),
        }
    }
}
