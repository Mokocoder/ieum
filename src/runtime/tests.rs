use super::*;
use crate::diagnostic::ExecutionError;
use crate::syntax::{self, format_source};

fn run(source: &str, options: &Options) -> (String, Option<&'static str>) {
    let mut output = Vec::new();
    let result = (|| {
        let parsed = syntax::parse_valid(source.as_bytes())?;
        let program = Program::prepare(parsed.statements).map_err(|errors| {
            errors
                .into_iter()
                .next()
                .expect("오류가 하나 이상 있습니다")
        })?;
        let mut heap = Heap::new(Limits::default())?;
        heap.stress_gc = true;
        let mut vm = program.vm(heap, &mut output, options)?;
        vm.run().or_else(ExecutionError::unless_success)?;
        vm.release();
        let roots: Vec<Handle> = vm.roots().collect();
        vm.heap.collect(roots.into_iter(), Span::default())?;
        assert_eq!(vm.heap.stats().live, 0, "{source}");
        Ok::<(), ExecutionError>(())
    })();
    let code = result.err().map(|error| error.diagnostic.code);
    (String::from_utf8(output).unwrap(), code)
}

fn check_table(table: &str, options: &Options) {
    for case in table.split("=== ").skip(1) {
        let (title, rest) = case.split_once('\n').unwrap();
        let (source, expected) = rest.split_once("---\n").unwrap();
        let (output, code) = match expected.rsplit_once("! ") {
            Some((output, code)) if output.is_empty() || output.ends_with('\n') => {
                (output, Some(code.trim_end()))
            }
            _ => (expected, None),
        };
        assert_eq!(run(source, options), (output.to_owned(), code), "{title}");
        if let Ok(formatted) = format_source(source.as_bytes()) {
            assert_eq!(
                run(&formatted, options),
                (output.to_owned(), code),
                "{title} 정리 후"
            );
        }
    }
}

#[test]
fn programs_print_what_the_table_says() {
    check_table(
        include_str!("../../tests/programs.txt"),
        &Options::default(),
    );
}

#[cfg(feature = "python")]
#[test]
fn python_programs_print_what_the_table_says() {
    let options = Options {
        system: true,
        ..Options::default()
    };
    check_table(include_str!("../../tests/python.txt"), &options);
}
