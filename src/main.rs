//! GUI for the virtual RC joystick.
//!
//! The GUI only edits the shared [`Controller`]; a separate output thread
//! samples it every 10 ms and writes to the uinput device. That keeps the
//! 4.5 s arm hold accurate even when the window is not being repainted
//! (for example while RealFlight covers it).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Vec2};
use virtual_joystick::device::{DEVICE_NAME, VirtualJoystick};
use virtual_joystick::sticks::{ARM_HOLD, Axis, Controller, SequenceKind, Sweep};

const OUTPUT_PERIOD: Duration = Duration::from_millis(10);

fn main() -> eframe::Result {
    let controller = Arc::new(Mutex::new(Controller::new()));

    let device = match VirtualJoystick::create() {
        Ok(joy) => {
            let node = joy.node.clone();
            spawn_output_thread(joy, Arc::clone(&controller));
            Ok(node)
        }
        Err(e) => {
            eprintln!("Could not create the virtual joystick: {e}");
            Err(e.to_string())
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(DEVICE_NAME)
            .with_app_id("virtual-rc-joystick")
            .with_inner_size([420.0, 640.0])
            .with_min_inner_size([380.0, 560.0])
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native(
        DEVICE_NAME,
        options,
        Box::new(|_cc| {
            Ok(Box::new(App { controller, device, spring_pitch: true, sweep: Sweep::All }))
        }),
    )
}

fn spawn_output_thread(mut joy: VirtualJoystick, controller: Arc<Mutex<Controller>>) {
    thread::Builder::new()
        .name("joystick-output".into())
        .spawn(move || {
            let mut error_reported = false;
            loop {
                let sticks = controller.lock().unwrap().output(Instant::now());
                if let Err(e) = joy.send(sticks)
                    && !error_reported
                {
                    eprintln!("Writing to the virtual joystick failed: {e}");
                    error_reported = true;
                }
                thread::sleep(OUTPUT_PERIOD);
            }
        })
        .expect("failed to start the joystick output thread");
}

struct App {
    controller: Arc<Mutex<Controller>>,
    /// Event node of the virtual joystick, or why it could not be created.
    device: Result<Option<PathBuf>, String>,
    /// Pitch returns to centre when the slider is let go, like a real stick.
    spring_pitch: bool,
    /// What the calibration sweep button moves.
    sweep: Sweep,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let controller = Arc::clone(&self.controller);
        let mut c = controller.lock().unwrap();
        let now = Instant::now();
        let active = c.active(now);
        let sliders_enabled = active.is_none();

        egui::CentralPanel::default_margins().show(ui, |ui| {
            device_banner(ui, &self.device);
            ui.add_space(10.0);

            // --- Sliders --------------------------------------------------
            ui.scope(|ui| {
                // Tall, thick faders that are easy to grab with the mouse.
                ui.spacing_mut().slider_width = 300.0;
                ui.spacing_mut().slider_rail_height = 12.0;
                ui.spacing_mut().interact_size.y = 36.0;
                ui.columns(2, |cols| {
                    cols[0].vertical_centered(|ui| {
                        ui.label(RichText::new("THROTTLE").strong().size(16.0));
                        ui.label("0 – 100 %");
                        let mut pct = c.throttle * 100.0;
                        let slider = egui::Slider::new(&mut pct, 0.0..=100.0)
                            .vertical()
                            .show_value(false)
                            .trailing_fill(true);
                        if centred_fader(ui, sliders_enabled, slider).changed() {
                            c.throttle = pct / 100.0;
                        }
                        ui.label(fader_value(format!("{:.0} %", c.throttle * 100.0)));
                    });
                    cols[1].vertical_centered(|ui| {
                        ui.label(RichText::new("PITCH").strong().size(16.0));
                        ui.label("▲ forward   ▼ back");
                        let mut pct = c.pitch * 100.0;
                        let slider = egui::Slider::new(&mut pct, -100.0..=100.0)
                            .vertical()
                            .show_value(false);
                        let response = centred_fader(ui, sliders_enabled, slider);
                        if response.changed() {
                            c.pitch = pct / 100.0;
                        }
                        if self.spring_pitch && !response.is_pointer_button_down_on() {
                            c.pitch = 0.0;
                        }
                        ui.label(fader_value(format!("{:+.0} %", c.pitch * 100.0)));
                    });
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.spring_pitch, "Pitch springs back to centre when released");
                if ui.add_enabled(sliders_enabled, egui::Button::new("Centre")).clicked() {
                    c.pitch = 0.0;
                }
            });
            ui.separator();

            // --- Arm / disarm ---------------------------------------------
            ui.horizontal(|ui| {
                let size = Vec2::new((ui.available_width() - 8.0) / 2.0, 52.0);
                let hold = ARM_HOLD.as_secs_f32();
                let arm = sequence_button("ARM", Color32::from_rgb(30, 130, 60), size);
                if ui
                    .add_enabled(sliders_enabled, arm)
                    .on_hover_text(format!("Holds throttle down + yaw right for {hold:.1} s"))
                    .clicked()
                {
                    c.start(SequenceKind::Arm, now);
                }
                let disarm = sequence_button("DISARM", Color32::from_rgb(170, 45, 45), size);
                if ui
                    .add_enabled(sliders_enabled, disarm)
                    .on_hover_text(format!("Holds throttle down + yaw left for {hold:.1} s"))
                    .clicked()
                {
                    c.start(SequenceKind::Disarm, now);
                }
            });
            ui.add_space(4.0);

            match active {
                Some(a) => {
                    ui.label(match a.kind {
                        SequenceKind::Arm => "Arming: throttle down, yaw right…".into(),
                        SequenceKind::Disarm => "Disarming: throttle down, yaw left…".into(),
                        SequenceKind::Calibrate(Sweep::All) => {
                            "Calibration sweep: every axis end to end…".into()
                        }
                        SequenceKind::Calibrate(sweep) => format!(
                            "Calibration sweep: {} end to end, other axes held…",
                            sweep_label(sweep)
                        ),
                    });
                    ui.horizontal(|ui| {
                        let text = format!(
                            "{:.1} / {:.1} s",
                            a.elapsed.as_secs_f32(),
                            a.kind.duration().as_secs_f32()
                        );
                        let bar = egui::ProgressBar::new(a.progress()).text(text);
                        ui.add_sized([ui.available_width() - 80.0, 20.0], bar);
                        if ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            c.cancel();
                        }
                    });
                }
                None => {
                    ui.label(match c.last_completed() {
                        Some(SequenceKind::Arm) => {
                            "Arm sequence sent: the quad should be armed.".into()
                        }
                        Some(SequenceKind::Disarm) => "Disarm sequence sent.".into(),
                        Some(SequenceKind::Calibrate(Sweep::All)) => {
                            "Calibration sweep finished.".into()
                        }
                        Some(SequenceKind::Calibrate(sweep)) => {
                            format!("{} sweep finished.", sweep_label(sweep))
                        }
                        None => "Ready.".to_string(),
                    });
                    ui.add_space(20.0);
                }
            }
            ui.separator();

            // --- Calibration + live output --------------------------------
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("sweep")
                    .selected_text(sweep_label(self.sweep))
                    .show_ui(ui, |ui| {
                        for choice in Sweep::CHOICES {
                            ui.selectable_value(&mut self.sweep, choice, sweep_label(choice));
                        }
                    })
                    .response
                    .on_hover_text("What the calibration sweep moves");
                if ui
                    .add_enabled(sliders_enabled, egui::Button::new("Calibration sweep"))
                    .on_hover_text(
                        "Moves the chosen axis (or all four) end to end three times. \
                         Sweep one axis to find and map it in RealFlight: the others \
                         stay centred and the throttle at idle. Sweep all axes for \
                         RealFlight's calibration wizard.",
                    )
                    .clicked()
                {
                    c.start(SequenceKind::Calibrate(self.sweep), now);
                }
            });
            ui.add_space(4.0);
            let out = c.output(now);
            ui.label(
                RichText::new(format!(
                    "Out: roll {:+.0}%  pitch {:+.0}%  thr {:.0}%  yaw {:+.0}%",
                    out.roll * 100.0,
                    out.pitch * 100.0,
                    out.throttle * 100.0,
                    out.yaw * 100.0
                ))
                .monospace(),
            );
        });

        // A sequence may have just started or still be running: keep the
        // progress bar moving until it is done.
        if c.active(Instant::now()).is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
    }
}

/// Adds a vertical slider centred in the column. (A vertical slider lays
/// itself out left-aligned, so `vertical_centered` alone does not centre it.)
fn centred_fader(ui: &mut egui::Ui, enabled: bool, slider: egui::Slider<'_>) -> egui::Response {
    let thickness = ui.spacing().interact_size.y;
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - thickness) / 2.0).max(0.0));
        ui.add_enabled(enabled, slider)
    })
    .inner
}

fn fader_value(text: String) -> RichText {
    RichText::new(text).size(18.0).monospace()
}

/// Name of a sweep choice, with the joystick axis it moves (see
/// `device::AXES`), so it can be found in RealFlight's channel list.
fn sweep_label(sweep: Sweep) -> &'static str {
    match sweep {
        Sweep::All => "All axes",
        Sweep::Only(Axis::Roll) => "Roll (axis 1, X)",
        Sweep::Only(Axis::Pitch) => "Pitch (axis 2, Y)",
        Sweep::Only(Axis::Throttle) => "Throttle (axis 3, Z)",
        Sweep::Only(Axis::Yaw) => "Yaw (axis 4, RX)",
    }
}

fn sequence_button(label: &str, fill: Color32, size: Vec2) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).size(20.0).strong().color(Color32::WHITE))
        .fill(fill)
        .min_size(size)
}

fn device_banner(ui: &mut egui::Ui, device: &Result<Option<PathBuf>, String>) {
    match device {
        Ok(node) => {
            let at = node.as_ref().map(|p| format!(" on {}", p.display())).unwrap_or_default();
            ui.label(
                RichText::new(format!("● {DEVICE_NAME} active{at}"))
                    .color(Color32::from_rgb(40, 170, 80)),
            );
        }
        Err(e) => {
            ui.label(
                RichText::new(format!("Could not create the virtual joystick: {e}"))
                    .color(Color32::from_rgb(220, 60, 60))
                    .strong(),
            );
            ui.label("It needs write access to /dev/uinput. See README.md.");
        }
    }
}
