use eframe::egui;
use std::process::Command;

struct UpdatePromptApp {
    repo_root: String,
    update_command: String,
    status: String,
}

impl UpdatePromptApp {
    fn new(repo_root: String) -> Self {
        let quoted_repo = shell_quote(&repo_root);
        let update_command = format!("cd {} && bash scripts/update-driver.sh", quoted_repo);
        Self {
            repo_root,
            update_command,
            status: String::new(),
        }
    }

    fn request_update(&mut self) {
        let spawn_result = Command::new("pkexec")
            .arg("bash")
            .arg("-lc")
            .arg(&self.update_command)
            .spawn();

        match spawn_result {
            Ok(_) => {
                self.status = "Update launched. Approve the system permission prompt to continue."
                    .to_string();
            }
            Err(err) => {
                self.status = format!(
                    "Could not launch automatic update ({err}). Run this manually:\n{}",
                    self.update_command
                );
            }
        }
    }
}

impl eframe::App for UpdatePromptApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("PTree update required");
            ui.separator();
            ui.label("Automatic update after wake-up failed.");
            ui.label("Do you want to run the update now?");
            ui.add_space(8.0);
            ui.label("If you prefer manual update, run:");
            ui.monospace(&self.update_command);
            ui.add_space(8.0);
            ui.label(format!("Repository: {}", self.repo_root));

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Yes, update now").clicked() {
                    self.request_update();
                }
                if ui.button("Not now").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            if !self.status.is_empty() {
                ui.add_space(12.0);
                ui.separator();
                ui.label(&self.status);
            }
        });
    }
}

fn shell_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

fn parse_repo_arg() -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--repo" {
            if let Some(value) = args.next() {
                return value;
            }
        }
    }

    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn main() -> eframe::Result<()> {
    let repo_root = parse_repo_arg();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PTree Update Permission")
            .with_inner_size([540.0, 240.0]),
        ..Default::default()
    };

    eframe::run_native(
        "PTree Update Permission",
        options,
        Box::new(|_cc| Box::new(UpdatePromptApp::new(repo_root))),
    )
}
