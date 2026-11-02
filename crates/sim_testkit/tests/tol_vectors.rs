//! The Rust `close` agrees with every shared vector that `saber.testing.close` is also checked
//! against (`verification-and-test-strategy.md` §3.2).

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(untagged)]
enum Num {
    F(f64),
    S(String),
}

impl Num {
    fn value(&self) -> f64 {
        match self {
            Num::F(x) => *x,
            Num::S(s) => s.parse().expect("inf, -inf or nan"),
        }
    }
}

#[derive(Deserialize)]
struct Case {
    a: Num,
    b: Num,
    atol: f64,
    rtol: f64,
    close: bool,
}

#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

#[test]
fn shared_vectors() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/shared/tol_vectors.json"
    );
    let v: Vectors = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(v.cases.len() >= 20);
    for (i, c) in v.cases.iter().enumerate() {
        let (a, b) = (c.a.value(), c.b.value());
        assert_eq!(
            sim_testkit::close(a, b, c.atol, c.rtol),
            c.close,
            "case {i}: close({a}, {b}, {}, {})",
            c.atol,
            c.rtol
        );
        assert_eq!(
            sim_testkit::close(b, a, c.atol, c.rtol),
            c.close,
            "case {i} is not symmetric"
        );
    }
}
