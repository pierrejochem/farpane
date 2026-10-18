//! The first-run wizard's window: three questions and a summary. What the
//! answers mean and how they are written is in [`crate::wizard`].

use egui::{Align, Context, Frame, Layout, Margin, RichText, Ui};
use farpane_ui::{Tokens, style, widgets};

use super::{Dashboard, Page, eyebrow, heading};
use crate::{
    backend::{Applied, Level},
    wizard::{Access, Choices},
};

/// Where the wizard is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Access,
    Account,
    Protection,
    Done,
}

/// The wizard while it is open.
#[derive(Debug)]
pub(super) struct Wizard {
    step: Step,
    choices: Choices,
    /// A problem to show under the current step.
    problem: Option<String>,
    /// What happened when the answers were applied (shown on the last step).
    outcome: Option<(bool, String)>,
}

impl Wizard {
    pub(super) fn new() -> Self {
        Self {
            step: Step::Access,
            choices: Choices::new(),
            problem: None,
            outcome: None,
        }
    }
}

/// The widest the card grows.
const WIDTH: f32 = 560.0;

impl Dashboard {
    /// Show the first-run wizard instead of the pages until it is finished or
    /// skipped. Call once at start when [`crate::wizard::is_first_run`] says
    /// the server has not been set up.
    pub fn start_first_run_wizard(&mut self) {
        self.wizard = Some(Wizard::new());
    }

    /// Whether the wizard is open.
    #[must_use]
    pub fn wizard_open(&self) -> bool {
        self.wizard.is_some()
    }

    pub(super) fn show_wizard(&mut self, ctx: &Context, tokens: &Tokens) {
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(style::page_fill(tokens))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.set_max_width(WIDTH);
                            Self::card(ui, tokens, |ui| self.wizard_card(ui, tokens));
                        });
                    });
            });
    }

    fn wizard_card(&mut self, ui: &mut Ui, tokens: &Tokens) {
        let Some(mut wizard) = self.wizard.take() else {
            return;
        };
        let mut close = false;
        ui.with_layout(Layout::top_down(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            close = match wizard.step {
                Step::Access => self.step_access(ui, tokens, &mut wizard),
                Step::Account => {
                    Self::step_account(ui, tokens, &mut wizard);
                    false
                }
                Step::Protection => {
                    self.step_protection(ui, tokens, &mut wizard);
                    false
                }
                Step::Done => self.step_done(ui, tokens, &wizard),
            };
        });
        if !close {
            self.wizard = Some(wizard);
        }
    }

    fn head(ui: &mut Ui, tokens: &Tokens, step: usize, title: &str, lead: &str) {
        ui.label(eyebrow(&format!("Step {step} of 3"), tokens));
        ui.label(heading(ui, title).size(20.0));
        ui.label(RichText::new(lead).color(tokens.muted).size(13.0));
        ui.add_space(6.0);
    }

    /// A grey explanation, indented under the control it belongs to.
    fn note(ui: &mut Ui, tokens: &Tokens, text: &str) {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(24.0);
            ui.label(RichText::new(text).color(tokens.muted).size(12.5));
        });
    }

    fn problem(ui: &mut Ui, tokens: &Tokens, wizard: &Wizard) {
        if let Some(p) = &wizard.problem {
            ui.label(RichText::new(p).color(tokens.red));
        }
    }

    /// Returns true when the wizard was skipped.
    fn step_access(&mut self, ui: &mut Ui, tokens: &Tokens, wizard: &mut Wizard) -> bool {
        Self::head(
            ui,
            tokens,
            1,
            "Welcome to Farpane Server",
            "Share this computer's screen with a viewer. Two questions and an account, then you are done.",
        );
        ui.label(heading(ui, "Who may connect?"));
        let mut access = wizard.choices.access;
        ui.radio_value(&mut access, Access::ThisComputer, "Only this computer");
        Self::note(
            ui,
            tokens,
            "For trying it out. Nothing on the network can reach the server.",
        );
        ui.radio_value(
            &mut access,
            Access::Network,
            "Other computers on my network",
        );
        Self::note(
            ui,
            tokens,
            "Viewers sign in with an account and the connection is encrypted. \
             You can change this later on the Security page.",
        );
        wizard.choices.set_access(access);
        ui.add_space(8.0);
        let mut skipped = false;
        ui.horizontal(|ui| {
            if ui.button("Skip for now").clicked() {
                self.log.push(
                    Level::Info,
                    "first-run setup skipped; it is offered again at the next start",
                );
                skipped = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Next").clicked() {
                    wizard.step = Step::Account;
                    wizard.problem = None;
                }
            });
        });
        skipped
    }

    fn step_account(ui: &mut Ui, tokens: &Tokens, wizard: &mut Wizard) {
        Self::head(
            ui,
            tokens,
            2,
            "Create the administrator account",
            "Viewers sign in with an account. There are no default accounts or passwords; this one is the first, and it can manage the others.",
        );
        egui::Grid::new("wizard-account")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("User name");
                ui.add(widgets::text_field(&mut wizard.choices.username));
                ui.end_row();
                ui.label("Display name");
                ui.add(widgets::text_field(&mut wizard.choices.display_name).hint_text("optional"));
                ui.end_row();
                ui.label("Password");
                ui.add(widgets::text_field(&mut wizard.choices.password).password(true));
                ui.end_row();
                ui.label("Repeat password");
                ui.add(widgets::text_field(&mut wizard.choices.repeat).password(true));
                ui.end_row();
            });
        ui.label(
            RichText::new("At least 8 characters. Use something you do not use anywhere else.")
                .color(tokens.muted)
                .size(12.5),
        );
        Self::problem(ui, tokens, wizard);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Back").clicked() {
                wizard.step = Step::Access;
                wizard.problem = None;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Next").clicked() {
                    wizard.problem = wizard.choices.account_problem();
                    if wizard.problem.is_none() {
                        wizard.step = Step::Protection;
                    }
                }
            });
        });
    }

    fn step_protection(&mut self, ui: &mut Ui, tokens: &Tokens, wizard: &mut Wizard) {
        Self::head(
            ui,
            tokens,
            3,
            "Protection",
            "Connections are always encrypted with TLS, using a certificate this server makes for itself. Each viewer asks its user to confirm the certificate's fingerprint the first time.",
        );
        ui.checkbox(
            &mut wizard.choices.require_approval,
            "Ask me before someone connects",
        );
        Self::note(
            ui,
            tokens,
            "The person at this computer allows or refuses each new connection. \
             Turn it off for a computer nobody sits at.",
        );
        ui.checkbox(&mut wizard.choices.ra2, "Also offer RSA-AES");
        Self::note(
            ui,
            tokens,
            "Another encrypted way to sign in, which some viewers prefer. TLS stays on.",
        );
        Self::problem(ui, tokens, wizard);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Back").clicked() {
                wizard.step = Step::Account;
                wizard.problem = None;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Finish setup").clicked() {
                    self.finish_wizard(wizard);
                }
            });
        });
    }

    /// Write the account and the settings and put them to work.
    fn finish_wizard(&mut self, wizard: &mut Wizard) {
        match wizard
            .choices
            .apply(&self.users_file, &self.settings_path())
        {
            Err(e) => wizard.problem = Some(e),
            Ok(settings) => {
                self.log.push(
                    Level::Info,
                    format!(
                        "first-run setup finished: administrator {:?} created",
                        wizard.choices.username.trim()
                    ),
                );
                wizard.outcome = Some(match self.backend.apply_settings(&settings) {
                    Ok(Applied::Live) => {
                        self.status = self.backend.status();
                        (
                            true,
                            "The server is running with these settings.".to_owned(),
                        )
                    }
                    Ok(Applied::Restart) => (
                        true,
                        "Saved. Restart the server to apply the settings.".to_owned(),
                    ),
                    Err(why) => (
                        false,
                        format!("Saved, but not applied: {why} They apply at the next start."),
                    ),
                });
                // The passwords are not needed any more.
                wizard.choices.password.clear();
                wizard.choices.repeat.clear();
                wizard.problem = None;
                wizard.step = Step::Done;
                self.security.loaded = false;
                self.users.last_load = f64::NEG_INFINITY;
            }
        }
    }

    /// The last step; returns true when the wizard is to be closed.
    fn step_done(&mut self, ui: &mut Ui, tokens: &Tokens, wizard: &Wizard) -> bool {
        let Some((ok, text)) = &wizard.outcome else {
            return true;
        };
        ui.label(eyebrow("All set", tokens));
        ui.label(heading(ui, "Farpane Server is set up").size(20.0));
        let colour = if *ok { tokens.green } else { tokens.amber };
        ui.label(RichText::new(text).color(colour));
        ui.add_space(4.0);
        let settings = wizard.choices.settings();
        ui.label(format!(
            "Listening on {}",
            settings.listen.as_deref().unwrap_or("the default address")
        ));
        ui.label(format!("Administrator: {}", wizard.choices.username.trim()));
        ui.label(
            RichText::new(
                "The Security page shows the certificate fingerprint a viewer asks you to confirm.",
            )
            .color(tokens.muted)
            .size(12.5),
        );
        ui.add_space(8.0);
        let mut closed = false;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Open the dashboard").clicked() {
                self.set_page(Page::Overview);
                closed = true;
            }
        });
        closed
    }
}
