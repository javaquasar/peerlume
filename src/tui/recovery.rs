//! Terminal startup recovery before the normal torrent view is available.
use super::*;
use crate::control::frontend::{Client, Recovery};

pub(super) fn attach(
    terminal: &mut TerminalGuard,
    directory: PathBuf,
) -> io::Result<Option<Client>> {
    let mut recovery = Recovery::new(directory);
    recovery.retry()?;
    let mut last_message = String::new();
    let mut redraw = true;
    loop {
        if let Some(client) = recovery.poll() {
            return Ok(Some(client));
        }
        if redraw || last_message != recovery.message() {
            terminal.terminal.draw(|frame| render(frame, &recovery))?;
            last_message = recovery.message().to_owned();
            redraw = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Char('r') if !recovery.busy() => recovery.retry()?,
                KeyCode::Char('v') if !recovery.busy() => recovery.validate()?,
                _ => {}
            },
            Event::Resize(_, _) => redraw = true,
            _ => {}
        }
    }
}

fn render(frame: &mut Frame<'_>, recovery: &Recovery) {
    let lines = vec![
        Line::from(recovery.message()),
        Line::default(),
        Line::from("Profile directory"),
        Line::from(recovery.directory().display().to_string()),
        Line::default(),
        Line::from(if recovery.busy() {
            "q closes this terminal view"
        } else {
            "r retry · v validate configuration · q quit"
        }),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Background client recovery ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
        frame.area(),
    );
}
