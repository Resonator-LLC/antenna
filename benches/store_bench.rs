use criterion::{black_box, criterion_group, criterion_main, Criterion};

use antenna::store::RdfStore;

fn bench_insert_turtle(c: &mut Criterion) {
    c.bench_function("store_insert_100_triples", |b| {
        let store = RdfStore::open(None).unwrap();
        b.iter(|| {
            for i in 0..100 {
                let turtle = format!(
                    "<urn:bench:{}> a <urn:Type> ; <urn:val> \"{}\" .",
                    i, i
                );
                let _ = store.insert_turtle(black_box(&turtle));
            }
        })
    });
}

fn bench_sparql_select(c: &mut Criterion) {
    let store = RdfStore::open(None).unwrap();
    // Pre-populate with 1000 triples
    for i in 0..1000 {
        let turtle = format!(
            "<urn:item:{}> a <urn:Item> ; <urn:value> \"{}\" .",
            i, i
        );
        store.insert_turtle(&turtle).unwrap();
    }

    c.bench_function("sparql_select_all", |b| {
        b.iter(|| {
            black_box(
                store
                    .query("SELECT ?s ?v WHERE { ?s a <urn:Item> ; <urn:value> ?v }")
                    .unwrap(),
            );
        })
    });
}

fn bench_sparql_ask(c: &mut Criterion) {
    let store = RdfStore::open(None).unwrap();
    store
        .insert_turtle("<urn:x> a <urn:Foo> ; <urn:val> \"bar\" .")
        .unwrap();

    c.bench_function("sparql_ask", |b| {
        b.iter(|| {
            black_box(store.ask("ASK { <urn:x> a <urn:Foo> }").unwrap());
        })
    });
}

criterion_group!(benches, bench_insert_turtle, bench_sparql_select, bench_sparql_ask);
criterion_main!(benches);
