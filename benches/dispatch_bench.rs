use criterion::{black_box, criterion_group, criterion_main, Criterion};

use antenna::dispatch::{extract_property, extract_type, turtle_escape};

fn bench_extract_type(c: &mut Criterion) {
    let lines = [
        "[] a carrier:TextMessage ; carrier:text \"hello\" .",
        "[] a sp:Select ; sp:text \"SELECT * WHERE { ?s ?p ?o }\" .",
        "[] a carrier:Connected ; carrier:transport \"UDP\" .",
        "<urn:x> a <http://resonator.network/v2/antenna#Bookmark> ; rdfs:label \"test\" .",
        "# this is a comment",
        "@prefix foo: <http://example.org/> .",
    ];

    c.bench_function("extract_type", |b| {
        b.iter(|| {
            for line in &lines {
                black_box(extract_type(black_box(line)));
            }
        })
    });
}

fn bench_extract_property(c: &mut Criterion) {
    let lines = [
        ("[] a sp:Select ; sp:text \"SELECT ?s WHERE { ?s a carrier:TextMessage }\" .", "sp:text"),
        ("[] a carrier:SendMsg ; carrier:friendId 0 ; carrier:text \"hello world\" .", "carrier:text"),
        ("[] a carrier:SetNick ; carrier:nick \"mynode\" .", "carrier:nick"),
    ];

    c.bench_function("extract_property", |b| {
        b.iter(|| {
            for (line, prop) in &lines {
                black_box(extract_property(black_box(line), black_box(prop)));
            }
        })
    });
}

fn bench_turtle_escape(c: &mut Criterion) {
    let strings = [
        "simple text",
        "text with \"quotes\" and \\backslashes\\",
        "multi\nline\ntext\twith\ttabs",
        "a]b[c{d}e(f)g<h>i",
        &"x".repeat(1000),
    ];

    c.bench_function("turtle_escape", |b| {
        b.iter(|| {
            for s in &strings {
                black_box(turtle_escape(black_box(s)));
            }
        })
    });
}

criterion_group!(benches, bench_extract_type, bench_extract_property, bench_turtle_escape);
criterion_main!(benches);
