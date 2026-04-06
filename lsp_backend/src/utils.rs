use std::{rc::Rc, sync::Arc};

pub(crate) trait UTF16Len {
    fn utf16_len(&self) -> usize;
}

impl UTF16Len for String {
    fn utf16_len(&self) -> usize
    {
        self.encode_utf16().count()
    }
}

impl UTF16Len for Arc<str> {
    fn utf16_len(&self) -> usize
    {
        self.encode_utf16().count()
    }
}

impl UTF16Len for Rc<str> {
    fn utf16_len(&self) -> usize
    {
        self.encode_utf16().count()
    }
}

pub fn get_file_content(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}