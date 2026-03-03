use core::fmt::{self, Write};

struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let _ = crate::write(1, s.as_bytes());
        Ok(())
    }
}

pub fn print(args: fmt::Arguments<'_>) {
    let _ = Stdout.write_fmt(args);
}
