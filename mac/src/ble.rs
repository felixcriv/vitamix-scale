//! Bluetooth side: finds the Vitamix / Perfect Company scale, keeps it connected and
//! streams weight and zero-point updates to the UI. Runs on its own thread with a tokio runtime.
//!
//! Protocol (worked out by probing the scale, firmware 1.3.3):
//!   service 1bc50001-…  char 1bc50002 = weight, int32 LE, milligrams (notify on change)
//!                       char 1bc50006 = scale's zero point, int32 LE, milligrams
//!   grams = (weight - zero) / 1000
//!   service b5600001-…  char b5600008 = [int32 0][int32 limit][uint16 flag];
//!                       writing the flag as 01 00 and then 00 00 makes the scale tare itself.

use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use std::time::Duration;
use uuid::{uuid, Uuid};

pub const WEIGHT: Uuid = uuid!("1bc50002-0200-0aa5-e311-24cb004a98c5");
pub const ZERO: Uuid = uuid!("1bc50006-0200-0aa5-e311-24cb004a98c5");
pub const TARE: Uuid = uuid!("b5600008-a0f8-16af-bb42-1d3b642ec2e1");
const BATTERY_LEVEL: Uuid = uuid!("00002a19-0000-1000-8000-00805f9b34fb");
const DFU_SERVICE: Uuid = uuid!("00001530-1212-efde-1523-785feabcd123");

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting,
    NoBluetooth(String),
    Searching,
    Connecting(String),
    Connected(String),
}

#[derive(Debug)]
pub enum Event {
    Status(Status),
    Weight(i32),
    Zero(i32),
    Battery(u8),
    Log(String),
}

#[derive(Debug)]
pub enum Command {
    Tare,
}

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Start the Bluetooth worker. Returns a sender for commands and a receiver for events.
pub fn start() -> (UnboundedSender<Command>, UnboundedReceiver<Event>) {
    let (ev_tx, ev_rx) = mpsc::unbounded();
    let (cmd_tx, cmd_rx) = mpsc::unbounded();
    std::thread::Builder::new()
        .name("bluetooth".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(run(ev_tx, cmd_rx));
        })
        .expect("bluetooth thread");
    (cmd_tx, ev_rx)
}

fn send(tx: &UnboundedSender<Event>, ev: Event) {
    let _ = tx.unbounded_send(ev);
}

async fn run(tx: UnboundedSender<Event>, mut cmds: UnboundedReceiver<Command>) {
    send(&tx, Event::Status(Status::Starting));
    let manager = loop {
        match Manager::new().await {
            Ok(m) => break m,
            Err(e) => {
                send(&tx, Event::Status(Status::NoBluetooth(e.to_string())));
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    };
    let central = loop {
        match manager.adapters().await.ok().and_then(|a| a.into_iter().next()) {
            Some(c) => break c,
            None => {
                send(&tx, Event::Status(Status::NoBluetooth("No Bluetooth adapter found".into())));
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    };

    loop {
        send(&tx, Event::Status(Status::Searching));
        let peripheral = match find_scale(&central).await {
            Ok(p) => p,
            Err(e) => {
                send(&tx, Event::Log(format!("Scan failed: {e}")));
                send(&tx, Event::Status(Status::NoBluetooth(
                    "Bluetooth is off, or this app isn't allowed to use it".into(),
                )));
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };
        let name = scale_name(&peripheral).await.unwrap_or_else(|| "Vitamix Scale".into());
        send(&tx, Event::Status(Status::Connecting(name.clone())));
        match session(&peripheral, &name, &tx, &mut cmds).await {
            Ok(()) => return, // UI went away
            Err(e) => send(&tx, Event::Log(format!("Connection ended: {e}"))),
        }
        let _ = peripheral.disconnect().await;
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
}

async fn scale_name(p: &Peripheral) -> Option<String> {
    let props = p.properties().await.ok().flatten()?;
    props.local_name.or(props.advertisement_name)
}

async fn is_scale(p: &Peripheral) -> bool {
    scale_name(p)
        .await
        .map(|n| {
            let n = n.to_lowercase();
            n.contains("vitamix") || n.contains("perfect")
        })
        .unwrap_or(false)
}

/// Scan until a scale shows up, then try to connect to it.
async fn find_scale(central: &Adapter) -> Res<Peripheral> {
    let mut events = central.events().await?;
    central.start_scan(ScanFilter::default()).await?;
    loop {
        for p in central.peripherals().await? {
            if is_scale(&p).await {
                match p.connect_with_timeout(Duration::from_secs(8)).await {
                    Ok(()) => {
                        let _ = central.stop_scan().await;
                        return Ok(p);
                    }
                    Err(_) => continue, // stale entry or out of range; keep scanning
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), events.next()).await;
    }
}

fn i32_le(v: &[u8]) -> Option<i32> {
    Some(i32::from_le_bytes(v.get(..4)?.try_into().ok()?))
}

async fn session(
    p: &Peripheral,
    name: &str,
    tx: &UnboundedSender<Event>,
    cmds: &mut UnboundedReceiver<Command>,
) -> Res<()> {
    p.discover_services().await?;
    let chars = p.characteristics();
    let find = |u: Uuid| chars.iter().find(|c| c.uuid == u).cloned();
    let weight = find(WEIGHT).ok_or("this device has no weight data (not a Vitamix scale?)")?;
    let zero = find(ZERO);
    let tare = find(TARE);

    let mut notes = p.notifications().await?;
    // Subscribe to everything the scale streams (it may only stream while its raw channels are on).
    for c in chars
        .iter()
        .filter(|c| c.properties.intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE))
        .filter(|c| c.service_uuid != DFU_SERVICE)
    {
        let _ = p.subscribe(c).await;
    }
    if let Some(v) = p.read(&weight).await.ok().as_deref().and_then(i32_le) {
        send(tx, Event::Weight(v));
    }
    if let Some(z) = &zero {
        if let Some(v) = p.read(z).await.ok().as_deref().and_then(i32_le) {
            send(tx, Event::Zero(v));
        }
    }
    if let Some(b) = find(BATTERY_LEVEL) {
        if let Some(&v) = p.read(&b).await.ok().as_deref().and_then(|v| v.first()) {
            send(tx, Event::Battery(v));
        }
    }
    // Ignore taps that queued up while we weren't connected.
    while cmds.try_recv().is_ok() {}
    send(tx, Event::Status(Status::Connected(name.to_string())));

    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tokio::select! {
            n = notes.next() => match n {
                Some(n) if n.uuid == WEIGHT => if let Some(v) = i32_le(&n.value) { send(tx, Event::Weight(v)) },
                Some(n) if n.uuid == ZERO => if let Some(v) = i32_le(&n.value) { send(tx, Event::Zero(v)) },
                Some(n) if n.uuid == BATTERY_LEVEL => if let Some(&v) = n.value.first() { send(tx, Event::Battery(v)) },
                Some(_) => {}
                None => return Err("notification stream closed".into()),
            },
            c = cmds.next() => match c {
                Some(Command::Tare) => match &tare {
                    Some(t) => if let Err(e) = scale_tare(p, t).await { send(tx, Event::Log(format!("Tare failed: {e}"))) },
                    None => send(tx, Event::Log("This scale has no tare command".into())),
                },
                None => return Ok(()),
            },
            _ = tick.tick() => {
                if !p.is_connected().await.unwrap_or(false) {
                    return Err("disconnected".into());
                }
            }
        }
    }
}

/// Make the scale zero itself: flag 01 00, short pause, then flag 00 00.
async fn scale_tare(p: &Peripheral, c: &Characteristic) -> Res<()> {
    let cur = p.read(c).await?;
    if cur.len() != 10 {
        return Err(format!("unexpected tare register length {}", cur.len()).into());
    }
    let mut data = cur.clone();
    data[8] = 1;
    data[9] = 0;
    p.write(c, &data, WriteType::WithResponse).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    data[8] = 0;
    p.write(c, &data, WriteType::WithResponse).await?;
    Ok(())
}
