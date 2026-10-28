use super::*;

impl InputPrompt {
    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.value.len());
        while !self.value.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    pub(super) fn edit(&mut self, key: KeyEvent) {
        self.clamp_cursor();
        let previous = || {
            self.value[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(index, _)| index)
                .unwrap_or(0)
        };
        let next = || {
            self.cursor
                + self.value[self.cursor..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(0)
        };
        match key.code {
            KeyCode::Left => self.cursor = previous(),
            KeyCode::Right => self.cursor = next(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            KeyCode::Backspace => {
                let start = previous();
                self.value.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Delete => {
                let end = next();
                self.value.drain(self.cursor..end);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control() =>
            {
                if self.kind == PromptKind::DownloadMode {
                    self.value.clear();
                    self.cursor = 0;
                }
                if self.value.len() + character.len_utf8() <= 8192 {
                    self.value.insert(self.cursor, character);
                    self.cursor += character.len_utf8();
                }
            }
            _ => {}
        }
    }

    pub(super) fn window(&self, sensitive: bool, width: usize) -> (String, usize) {
        let mut cursor = self.cursor.min(self.value.len());
        while !self.value.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let display = if sensitive {
            "*".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        };
        if sensitive {
            cursor = self.value[..cursor].chars().count();
        }
        let mut start = cursor;
        let mut column = 0;
        for (index, character) in display[..cursor].char_indices().rev() {
            let size = Line::from(character.to_string()).width();
            if column + size > width {
                break;
            }
            column += size;
            start = index;
        }
        let mut output = String::new();
        let mut used = 0;
        for character in display[start..].chars() {
            let size = Line::from(character.to_string()).width();
            if used + size > width {
                break;
            }
            output.push(character);
            used += size;
        }
        (output, column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_insert_delete_movement_and_masking_preserve_character_boundaries() {
        let mut prompt = InputPrompt {
            kind: PromptKind::Preference,
            title: "value",
            value: "a日本z".into(),
            cursor: 1,
        };
        prompt.edit(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        prompt.edit(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(prompt.value, "a日z");
        prompt.edit(KeyEvent::new(KeyCode::Char('語'), KeyModifiers::NONE));
        assert_eq!(prompt.value, "a日語z");
        prompt.edit(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(prompt.value, "a日z");
        assert_eq!(prompt.window(false, 2), ("日".into(), 2));
        assert_eq!(prompt.window(true, 3), ("***".into(), 2));
        prompt.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(prompt.value.is_empty());
        prompt.value = "x".repeat(8192);
        prompt.cursor = 8192;
        prompt.edit(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(prompt.value.len(), 8192);
    }
}
