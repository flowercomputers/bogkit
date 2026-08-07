use catalog_compiler::Diagnostic;
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TrackingAllocator;

static LIVE_ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static PEAK_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

impl TrackingAllocator {
    fn add(size: usize) {
        let live = LIVE_ALLOCATED.fetch_add(size, Ordering::Relaxed) + size;
        let mut peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
        while live > peak {
            match PEAK_ALLOCATED.compare_exchange_weak(
                peak,
                live,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(previous) => peak = previous,
            }
        }
    }

    fn subtract(size: usize) {
        LIVE_ALLOCATED.fetch_sub(size, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            Self::add(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        Self::subtract(layout.size());
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !new_pointer.is_null() {
            if new_size >= layout.size() {
                Self::add(new_size - layout.size());
            } else {
                Self::subtract(layout.size() - new_size);
            }
        }
        new_pointer
    }
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let code = match args.next().as_deref() {
        Some("compile") => {
            let Some(input) = args.next() else {
                return usage();
            };
            let Some(output) = args.next() else {
                return usage();
            };
            compile(Path::new(&input), Path::new(&output))
        }
        Some("generate-stress") => {
            let Some(output) = args.next() else {
                return usage();
            };
            let messages = args.next().and_then(|n| n.parse().ok()).unwrap_or(100_000);
            let locales = args.next().and_then(|n| n.parse().ok()).unwrap_or(18);
            generate_stress(Path::new(&output), messages, locales)
        }
        Some("lookup") => {
            let Some(input) = args.next() else {
                return usage();
            };
            let Some(locale) = args.next() else {
                return usage();
            };
            let Some(id) = args.next() else {
                return usage();
            };
            lookup(Path::new(&input), &locale, &id)
        }
        Some("lookup-table") => {
            let Some(input) = args.next() else {
                return usage();
            };
            let Some(locale) = args.next() else {
                return usage();
            };
            let Some(id) = args.next() else {
                return usage();
            };
            lookup_table(Path::new(&input), &locale, &id)
        }
        _ => usage(),
    };
    if env::var_os("CATALOG_MEMORY_REPORT").is_some() {
        eprintln!(
            "peak_live_allocated_bytes={}",
            PEAK_ALLOCATED.load(Ordering::Relaxed)
        );
    }
    code
}

fn compile(input: &Path, output: &Path) -> ExitCode {
    match catalog_compiler::load_dir(input) {
        Ok(catalogs) => {
            if let Err(error) = fs::write(output, catalogs.emit_table()) {
                eprintln!("error: cannot write {}: {error}", output.display());
                ExitCode::from(1)
            } else {
                println!("compiled {} into {}", input.display(), output.display());
                ExitCode::SUCCESS
            }
        }
        Err(diagnostics) => report(diagnostics),
    }
}

fn lookup(input: &Path, locale: &str, id: &str) -> ExitCode {
    match catalog_compiler::load_dir(input) {
        Ok(catalogs) => render_lookup(catalogs, locale, id),
        Err(diagnostics) => report(diagnostics),
    }
}

fn lookup_table(input: &Path, locale: &str, id: &str) -> ExitCode {
    match catalog_compiler::load_table(input) {
        Ok(catalogs) => render_lookup(catalogs, locale, id),
        Err(diagnostics) => report(diagnostics),
    }
}

fn render_lookup(catalogs: catalog_compiler::CatalogSet, locale: &str, id: &str) -> ExitCode {
    let vars = HashMap::from([
        (String::from("name"), String::from("Ada")),
        (String::from("count"), String::from("2")),
        (String::from("gender"), String::from("other")),
    ]);
    match catalogs.lookup(locale, id, &vars) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn report(diagnostics: Vec<Diagnostic>) -> ExitCode {
    let count = diagnostics.len();
    for diagnostic in diagnostics {
        eprintln!("{}", diagnostic.render());
    }
    eprintln!("{count} diagnostic(s)");
    ExitCode::from(1)
}

fn generate_stress(output: &Path, message_count: usize, locale_count: usize) -> ExitCode {
    if message_count == 0 || locale_count == 0 || locale_count > 26 {
        eprintln!("error: message count must be positive and locale count must be 1..=26");
        return ExitCode::from(2);
    }
    if let Err(error) = fs::create_dir_all(output) {
        eprintln!("error: cannot create {}: {error}", output.display());
        return ExitCode::from(1);
    }
    let locales = (0..locale_count)
        .map(|index| {
            if index == 0 {
                "en-US".to_owned()
            } else {
                format!("x-{index:02}")
            }
        })
        .collect::<Vec<_>>();
    let per_locale = message_count / locale_count;
    let remainder = message_count % locale_count;
    for (locale_index, locale) in locales.iter().enumerate() {
        let count = per_locale + usize::from(locale_index < remainder);
        let mut text = format!(
            "locale {locale}\nfallback {}\n",
            if locale_index == 0 { "-" } else { "en-US" }
        );
        for message_id in 0..count {
            text.push_str(&format!(
                "message msg{message_id:06}\ntext {locale} {message_id} for {{name}}\n"
            ));
        }
        if let Err(error) = fs::write(output.join(format!("{locale}.cat")), text) {
            eprintln!("error: cannot write stress fixture: {error}");
            return ExitCode::from(1);
        }
    }
    println!(
        "generated {message_count} messages across {locale_count} locales in {}",
        output.display()
    );
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: catalogc compile <catalog-dir> <table-file>\n\
         catalogc lookup <catalog-dir> <locale> <message-id>\n\
         catalogc lookup-table <table-file> <locale> <message-id>\n\
         catalogc generate-stress <dir> [messages] [locales]"
    );
    ExitCode::from(2)
}
