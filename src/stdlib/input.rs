use std::cell::RefCell;
use std::io::{self, BufRead};

pub trait Input {
    fn read_line(&self) -> io::Result<Option<String>>;
}

impl Input for io::Stdin {
    fn read_line(&self) -> io::Result<Option<String>> {
        line(|buffer| io::Stdin::read_line(self, buffer))
    }
}

impl<R: BufRead> Input for RefCell<R> {
    fn read_line(&self) -> io::Result<Option<String>> {
        line(|buffer| self.borrow_mut().read_line(buffer))
    }
}

fn line(read: impl FnOnce(&mut String) -> io::Result<usize>) -> io::Result<Option<String>> {
    let mut buffer = String::new();
    if read(&mut buffer)? == 0 {
        return Ok(None);
    }
    if buffer.ends_with('\n') {
        buffer.pop();
        if buffer.ends_with('\r') {
            buffer.pop();
        }
    }
    Ok(Some(buffer))
}
