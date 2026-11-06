//! Trusted user controls. These actions are intentionally absent from IPC/MCP.
use super::{Controller, access::Grants};

#[derive(Default)]
pub(crate) struct AccessPanel {
    pub open: bool,
    #[cfg(feature = "gui")]
    pub compact: bool,
    pub grants: Grants,
    pub token: String,
    pub status: String,
    remote_refreshed: Option<std::time::Instant>,
}

impl AccessPanel {
    pub(crate) fn poll(&mut self, control: &Controller) {
        if self.open
            && let Some(client) = control.backend.attached_client()
        {
            self.remote_refresh(client);
        }
    }
    fn remote_refresh(&mut self, client: &super::frontend::Client) {
        if let Some(token) = client.take_access_token() {
            self.token = token;
        }
        if self
            .remote_refreshed
            .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(1))
        {
            if let Err(error) = client.access_action(super::frontend::AccessAction::View) {
                self.status = error;
            }
            self.remote_refreshed = Some(std::time::Instant::now());
        }
    }
    fn remote_action(
        &mut self,
        client: &super::frontend::Client,
        action: super::frontend::AccessAction,
    ) {
        self.status = client
            .access_action(action)
            .map(|()| "Awaiting background owner confirmation".into())
            .unwrap_or_else(|error| error);
        self.remote_refreshed = None;
    }
    fn enable(control: &mut Controller, enabled: bool) -> Result<(), super::wire::ErrorCode> {
        let agent = control
            .agent_access()
            .ok_or(super::wire::ErrorCode::Disabled)?;
        if enabled {
            for root in [&agent.import_root, &agent.export_root] {
                std::fs::create_dir_all(root).map_err(|_| super::wire::ErrorCode::PathDenied)?;
            }
        }
        agent.enabled = enabled;
        Ok(())
    }

    #[cfg(feature = "gui")]
    pub fn show(&mut self, control: &mut Controller, ctx: &eframe::egui::Context) {
        if let Some(client) = control.backend.attached_client().cloned() {
            self.show_remote(&client, ctx);
            return;
        }
        use eframe::egui;
        if !self.compact {
            egui::TopBottomPanel::top("agent_access_button").show(ctx, |ui| {
                if ui.button("Agent access...").clicked() {
                    self.open = !self.open;
                }
                ui.label(if control.agent.as_ref().is_some_and(|a| a.enabled) {
                    "Local agents enabled"
                } else {
                    "Agent access disabled"
                });
            });
        }
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Local agent access").open(&mut open).show(ctx,|ui|{
            let enabled=control.agent.as_ref().is_some_and(|a|a.enabled);
            let mut requested=enabled;
            if ui.checkbox(&mut requested,"Enable local IPC for this application session").changed(){
                self.status=Self::enable(control,requested).err().map(|e|format!("{e:?}")).unwrap_or_default();
            }
            ui.label("New tokens are read-only unless you explicitly grant permissions below. No payload deletion or shell access.");
            ui.checkbox(&mut self.grants.manage,"Manage downloads (includes starting network transfers)");
            ui.checkbox(&mut self.grants.reporting,"Change experimental reporting policies");
            ui.checkbox(&mut self.grants.export,"Export redacted journal (per-operation approval)");
            ui.checkbox(&mut self.grants.remove,"Remove torrent, keep payload (per-operation approval)");
            let Some(agent)=control.agent_access() else {ui.label("Control store unavailable");return;};
            if ui.button("Create token with these grants").clicked(){
                match agent.grant(self.grants){Ok(token)=>self.token=token,Err(error)=>self.status=format!("{error:?}")}
            }
            if !self.token.is_empty(){
                ui.label("Copy this token now; it is not stored. Configure the adapter through a secret environment variable.");
                ui.add(egui::TextEdit::singleline(&mut self.token).password(true));
                if ui.button("Copy token").clicked(){ctx.copy_text(self.token.clone());}
                if ui.button("Forget displayed token").clicked(){self.token.clear();}
            }
            ui.label(format!("Import root: {}",agent.import_root.display()));
            ui.label(format!("Download root: {}",agent.download_root.display()));
            ui.label(format!("Export root: {}",agent.export_root.display()));
            let clients=agent.clients.clone();
            for client in clients {
                ui.horizontal(|ui|{
                    ui.label(format!("{} {:?}",client.id,client.grants));
                    if ui.button("Revoke").clicked(){let _=agent.revoke(&client.id);}
                });
            }
            for operation in agent.approvals(){
                ui.separator();ui.label(format!("{}: {}",operation.command,operation.torrent_id));
                ui.label(format!("Client {} / operation {}",operation.client_id,operation.id));
                if let Some(description)=agent.approval_description(&operation.id){ui.label(description);}
                ui.horizontal(|ui|{
                    if ui.button("Approve this exact operation").clicked(){let _=agent.approve(&operation.id,true);}
                    if ui.button("Deny").clicked(){let _=agent.approve(&operation.id,false);}
                });
            }
            ui.label(&self.status);
        });
        self.open = open;
    }

    #[cfg(feature = "gui")]
    fn show_remote(&mut self, client: &super::frontend::Client, ctx: &eframe::egui::Context) {
        use super::frontend::AccessAction;
        use eframe::egui;
        if !self.compact {
            egui::TopBottomPanel::top("agent_access_button").show(ctx, |ui| {
                if ui.button("Agent access...").clicked() {
                    self.open = !self.open;
                    self.remote_refreshed = None;
                }
                ui.label(if client.access_view().enabled {
                    "Local agents enabled"
                } else {
                    "Agent access disabled"
                });
            });
        }
        if !self.open {
            return;
        }
        self.remote_refresh(client);
        let view = client.access_view();
        let mut open = self.open;
        egui::Window::new("Local agent access").open(&mut open).show(ctx,|ui|{
            let mut enabled=view.enabled;
            if ui.checkbox(&mut enabled,"Enable local IPC for this background owner session").changed(){self.remote_action(client,AccessAction::Enable(enabled));}
            ui.label("New tokens are read-only unless you explicitly grant permissions below. No payload deletion or shell access.");
            ui.checkbox(&mut self.grants.manage,"Manage downloads (includes starting network transfers)");
            ui.checkbox(&mut self.grants.reporting,"Change experimental reporting policies");
            ui.checkbox(&mut self.grants.export,"Export redacted journal (per-operation approval)");
            ui.checkbox(&mut self.grants.remove,"Remove torrent, keep payload (per-operation approval)");
            if ui.button("Create token with these grants").clicked(){self.remote_action(client,AccessAction::Grant(self.grants));}
            if !self.token.is_empty(){
                ui.label("Copy this token now; it is not stored. Configure the adapter through a secret environment variable.");
                ui.add(egui::TextEdit::singleline(&mut self.token).password(true));
                if ui.button("Copy token").clicked(){ctx.copy_text(self.token.clone());}
                if ui.button("Forget displayed token").clicked(){self.token.clear();}
            }
            ui.label(format!("Import root: {}",view.import_root.display()));ui.label(format!("Download root: {}",view.download_root.display()));ui.label(format!("Export root: {}",view.export_root.display()));
            for grant in &view.clients{ui.horizontal(|ui|{ui.label(format!("{} {:?}",grant.id,grant.grants));if ui.button("Revoke").clicked(){self.remote_action(client,AccessAction::Revoke(grant.id.clone()));}});}
            for (operation,description) in &view.approvals{
                ui.separator();ui.label(format!("{}: {}",operation.command,operation.torrent_id));ui.label(format!("Client {} / operation {}",operation.client_id,operation.id));ui.label(description);
                ui.horizontal(|ui|{
                    if ui.button("Approve this exact operation").clicked(){self.remote_action(client,AccessAction::Approve{operation:operation.id.clone(),allow:true});}
                    if ui.button("Deny").clicked(){self.remote_action(client,AccessAction::Approve{operation:operation.id.clone(),allow:false});}
                });
            }
            ui.label(&self.status);
        });
        self.open = open;
    }

    #[cfg(feature = "tui")]
    pub fn key(&mut self, control: &mut Controller, key: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode;
        if let Some(client) = control.backend.attached_client().cloned() {
            use super::frontend::AccessAction;
            if key == KeyCode::F(8) {
                self.open = !self.open;
                self.remote_refreshed = None;
                if self.open {
                    self.remote_refresh(&client);
                }
                return true;
            }
            if !self.open {
                return false;
            }
            self.remote_refresh(&client);
            match key {
                KeyCode::Esc => {
                    self.open = false;
                    self.token.clear();
                }
                KeyCode::Char('e') => {
                    self.remote_action(&client, AccessAction::Enable(!client.access_view().enabled))
                }
                KeyCode::Char('m') => self.grants.manage = !self.grants.manage,
                KeyCode::Char('t') => self.grants.reporting = !self.grants.reporting,
                KeyCode::Char('j') => self.grants.export = !self.grants.export,
                KeyCode::Char('d') => self.grants.remove = !self.grants.remove,
                KeyCode::Char('g') => self.remote_action(&client, AccessAction::Grant(self.grants)),
                KeyCode::Char('x') => self.remote_action(&client, AccessAction::RevokeAll),
                KeyCode::Char('y' | 'n') => {
                    if let Some((operation, _)) = client.access_view().approvals.first() {
                        self.remote_action(
                            &client,
                            AccessAction::Approve {
                                operation: operation.id.clone(),
                                allow: key == KeyCode::Char('y'),
                            },
                        );
                    }
                }
                _ => {}
            }
            return true;
        }
        if key == KeyCode::F(8) {
            self.open = !self.open;
            return true;
        }
        if !self.open {
            return false;
        }
        match key {
            KeyCode::Esc => {
                self.open = false;
                self.token.clear();
            }
            KeyCode::Char('e') => {
                let enabled = control.agent.as_ref().is_some_and(|a| a.enabled);
                self.status = Self::enable(control, !enabled)
                    .err()
                    .map(|e| format!("{e:?}"))
                    .unwrap_or_default();
            }
            KeyCode::Char('m') => self.grants.manage = !self.grants.manage,
            KeyCode::Char('t') => self.grants.reporting = !self.grants.reporting,
            KeyCode::Char('j') => self.grants.export = !self.grants.export,
            KeyCode::Char('d') => self.grants.remove = !self.grants.remove,
            KeyCode::Char('g') => {
                if let Some(agent) = control.agent_access() {
                    match agent.grant(self.grants) {
                        Ok(token) => self.token = token,
                        Err(e) => self.status = format!("{e:?}"),
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(agent) = control.agent_access() {
                    for client in agent.clients.clone() {
                        let _ = agent.revoke(&client.id);
                    }
                }
            }
            KeyCode::Char('y' | 'n') => {
                if let Some(agent) = control.agent_access()
                    && let Some(operation) = agent.approvals().first()
                {
                    let _ = agent.approve(&operation.id, key == KeyCode::Char('y'));
                }
            }
            _ => {}
        }
        true
    }

    #[cfg(feature = "tui")]
    pub fn render(&self, control: &Controller, frame: &mut ratatui::Frame) {
        use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
        let mut text = format!(
            "F8/Esc close | e enable/disable | g create token | x revoke all\nm manage={} | t reporting={} | j export={} | d remove={}\n{}\n",
            self.grants.manage,
            self.grants.reporting,
            self.grants.export,
            self.grants.remove,
            self.status
        );
        if let Some(client) = control.backend.attached_client() {
            let view = client.access_view();
            text.push_str(&format!(
                "Local IPC enabled: {}. Clients: {}\nImports: {}\nDownloads: {}\nExports: {}\n",
                view.enabled,
                view.clients.len(),
                view.import_root.display(),
                view.download_root.display(),
                view.export_root.display()
            ));
            if !self.token.is_empty() {
                text.push_str(&format!(
                    "Token (displayed only here; copy now): {}\n",
                    self.token
                ));
            }
            for grant in &view.clients {
                text.push_str(&format!("Client {} {:?}\n", grant.id, grant.grants));
            }
            if let Some((operation, description)) = view.approvals.first() {
                text.push_str(&format!("y approve / n deny FIRST exact operation:\n{} {}\nClient {} / operation {}\n{}",operation.command,operation.torrent_id,operation.client_id,operation.id,description));
            }
        } else if let Some(agent) = &control.agent {
            text.push_str(&format!(
                "Local IPC enabled: {}. Clients: {}\n",
                agent.enabled,
                agent.clients.len()
            ));
            text.push_str(&format!(
                "Imports: {}\nDownloads: {}\nExports: {}\n",
                agent.import_root.display(),
                agent.download_root.display(),
                agent.export_root.display()
            ));
            if !self.token.is_empty() {
                text.push_str(&format!(
                    "Token (displayed only here; copy now): {}\n",
                    self.token
                ));
            }
            for client in &agent.clients {
                text.push_str(&format!("Client {} {:?}\n", client.id, client.grants));
            }
            if let Some(operation) = agent.approvals().first() {
                text.push_str(&format!("y approve / n deny FIRST exact operation:\n{} {}\nClient {} / operation {}\n{}",operation.command,operation.torrent_id,operation.client_id,operation.id,agent.approval_description(&operation.id).unwrap_or_default()));
            }
        }
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Trusted local agent access"),
            ),
            frame.area(),
        );
    }
}
