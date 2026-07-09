//! Wireless screen wiring — ONE shared set of WiFi credentials for every Pith
//! device, sent to each over its USB link (`@WIFI`; the device stores them in
//! NVS and joins the network on its own), plus the per-hardware wireless
//! toggles that decide what the link is used for (see `crate::wifi` for the
//! transport side: discovery, axis routing, DDU telemetry forwarding).

use std::sync::Arc;

use slint::ComponentHandle;

use crate::ctx::Ctx;
use crate::ui_bridge::sstr;
use crate::{AppWindow, Wireless};

/// The credentials to provision with: whatever is in the screen's fields right
/// now (so SEND works without a separate SAVE), persisted as a side effect.
/// `None` (+ a status hint) when no SSID has been entered.
fn take_creds(ctx: &Arc<Ctx>, ui: &AppWindow) -> Option<(String, String)> {
    let w = ui.global::<Wireless>();
    let ssid = w.get_ssid().trim().to_string();
    let pass = w.get_pass().to_string();
    if ssid.is_empty() {
        w.set_creds_status(sstr("Enter the network's SSID first"));
        return None;
    }
    let mut s = ctx.lock();
    if s.wifi_ssid != ssid || s.wifi_pass != pass {
        s.wifi_ssid = ssid.clone();
        s.wifi_pass = pass.clone();
        crate::persist::save_udp_cfg(&s);
    }
    Some((ssid, pass))
}

pub fn wire_wireless_callbacks(ui: &AppWindow, ctx: &Arc<Ctx>) {
    let w = ui.global::<Wireless>();

    // Seed the screen from the persisted state.
    {
        let s = ctx.lock();
        w.set_ssid(sstr(&s.wifi_ssid));
        w.set_pass(sstr(&s.wifi_pass));
        w.set_ddu_enabled(s.wifi_ddu_enabled);
        w.set_ddu_input_enabled(s.wifi_ddu_input);
        w.set_hb_input_enabled(s.wifi_hb_input);
        w.set_pedals_input_enabled(s.wifi_pedals_input);
    }

    w.on_save_credentials({
        let c = ctx.clone();
        move |ssid, _pass| {
            let Some(u) = c.ui.upgrade() else { return };
            if take_creds(&c, &u).is_some() {
                u.global::<Wireless>().set_creds_status(sstr(&format!(
                    "Saved \"{}\" — send it to each device below",
                    ssid.trim()
                )));
            }
        }
    });

    // ---- per-device provisioning (over the device's USB link) ----

    w.on_provision_ddu({
        let c = ctx.clone();
        move || {
            let Some(u) = c.ui.upgrade() else { return };
            let w = u.global::<Wireless>();
            let Some((ssid, pass)) = take_creds(&c, &u) else { return };
            if !c.dash().connected() {
                w.set_ddu_status(sstr("Connect the DDU over USB first"));
                return;
            }
            let (ok, _) = c.dash().command(&format!("@WIFI {ssid} {pass}"));
            w.set_ddu_status(sstr(&if ok {
                format!("WiFi credentials sent for \"{ssid}\"")
            } else {
                "WiFi provisioning failed — update the DDU firmware".to_string()
            }));
        }
    });

    w.on_provision_hb({
        let c = ctx.clone();
        move || {
            let Some(u) = c.ui.upgrade() else { return };
            let w = u.global::<Wireless>();
            let Some((ssid, pass)) = take_creds(&c, &u) else { return };
            if !u.global::<crate::Hb>().get_connected() {
                w.set_hb_status(sstr("Connect the handbrake over USB first"));
                return;
            }
            w.set_hb_status(sstr("Sending…"));
            c.send_hb(crate::hb::HbOutbound::ProvisionWifi {
                ssid,
                password: pass,
            });
        }
    });

    w.on_provision_pedal({
        let c = ctx.clone();
        move || {
            let Some(u) = c.ui.upgrade() else { return };
            let w = u.global::<Wireless>();
            let Some((ssid, pass)) = take_creds(&c, &u) else { return };
            if !u.global::<crate::Pedals>().get_connected() {
                w.set_pedal_status(sstr("Connect a pedal over USB first"));
                return;
            }
            w.set_pedal_status(sstr("Sending…"));
            c.send_pedals(crate::pedals::PedalsOutbound::ProvisionWifi {
                ssid,
                password: pass,
            });
        }
    });

    // ---- per-hardware wireless toggles (persisted; wifi.rs reads them) ----

    w.on_set_ddu_enabled({
        let c = ctx.clone();
        move |on| {
            let mut s = c.lock();
            s.wifi_ddu_enabled = on;
            crate::persist::save_udp_cfg(&s);
        }
    });

    w.on_set_ddu_input({
        let c = ctx.clone();
        move |on| {
            let mut s = c.lock();
            s.wifi_ddu_input = on;
            crate::persist::save_udp_cfg(&s);
        }
    });

    w.on_set_hb_input({
        let c = ctx.clone();
        move |on| {
            let mut s = c.lock();
            s.wifi_hb_input = on;
            crate::persist::save_udp_cfg(&s);
        }
    });

    w.on_set_pedals_input({
        let c = ctx.clone();
        move |on| {
            let mut s = c.lock();
            s.wifi_pedals_input = on;
            crate::persist::save_udp_cfg(&s);
        }
    });
}
