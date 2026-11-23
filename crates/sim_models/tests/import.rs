//! The fragment drag importer (IB-13, IB-16, build step 5e-2) on synthetic tables (TD1): each
//! convention converts to the SI ballistic coefficient exactly, the result drives the kernel's
//! `FragmentDrag`, and every malformed file is refused with its line.

use sim_models::import::ImportError;
use sim_models::import::fragment_drag::{LB_PER_IN2, fragment_drag};

#[test]
fn si_imperial_and_cd_tables_convert_to_kg_per_m2() {
    let si = fragment_drag(
        "# fragments of the test article\nMach, BC_kg_m2\n0.0, 200\n0.8,180\n 1.2 , 120\n3.0, 150\n",
    )
    .unwrap();
    assert_eq!(si.drag.mach, [0.0, 0.8, 1.2, 3.0]);
    assert_eq!(si.drag.ballistic_coefficient, [200.0, 180.0, 120.0, 150.0]);
    assert_eq!(si.convention, "bc_kg_m2 against Mach (4 rows)");
    assert_eq!(si.drag.at(1.0), 150.0);

    // 1 lb/in² = 0.45359237 kg / 0.00064516 m².
    assert!((LB_PER_IN2 - 703.069_579_639_159).abs() < 1e-9);
    let imp = fragment_drag("bc_lb_in2,mach\n0.25,0.5\n0.5,2.0\n").unwrap();
    assert_eq!(imp.drag.mach, [0.5, 2.0]);
    assert_eq!(
        imp.drag.ballistic_coefficient,
        [0.25 * LB_PER_IN2, 0.5 * LB_PER_IN2]
    );
    assert!(imp.convention.contains("converted to kg/m²"));

    // BC = (m / A) / Cd.
    let cd =
        fragment_drag("# mass_per_area_kg_m2: 120\nmach,cd\n0.0,0.6\n0.8,0.66\n1.2,1.0\n3.0,0.8\n")
            .unwrap();
    assert_eq!(
        cd.drag.ballistic_coefficient,
        [200.0, 120.0 / 0.66, 120.0, 150.0]
    );
    assert_eq!(
        cd.convention,
        "cd against Mach (4 rows) with mass_per_area 120 kg/m²"
    );
    let cd_imp =
        fragment_drag("# Mass_Per_Area_lb_in2: 0.5\n# drag_law: none\nmach, cd\n0, 0.5\n2, 0.25\n")
            .unwrap();
    assert_eq!(
        cd_imp.drag.ballistic_coefficient,
        [0.5 * LB_PER_IN2 / 0.5, 0.5 * LB_PER_IN2 / 0.25]
    );

    // One row, no Mach: a constant coefficient.
    let c = fragment_drag("bc_kg_m2\n75\n").unwrap();
    assert_eq!(c.drag, sim_core::FragmentDrag::constant(75.0));
    assert_eq!(c.convention, "bc_kg_m2 constant");
}

#[test]
fn malformed_tables_are_refused_with_their_line() {
    let err = |text: &str| fragment_drag(text).unwrap_err();
    let cases: [(&str, Option<usize>, &str); 13] = [
        ("", None, "no header row"),
        ("# only comments\n", None, "no header row"),
        ("mach,bc_kg_m2\n", None, "no data rows"),
        ("mach,speed\n0,1\n", Some(1), "unknown column"),
        ("mach,cd,bc_kg_m2\n0,1,2\n", Some(1), "more than one"),
        ("mach,mach,cd\n0,0,1\n", Some(1), "named twice"),
        ("mach\n0\n", Some(1), "no value column"),
        ("mach,bc_kg_m2\n0,100\n1\n", Some(3), "1 cells"),
        ("mach,bc_kg_m2\n0,abc\n", Some(2), "not a number"),
        ("mach,bc_kg_m2\n0,100\n1,-5\n", Some(3), "> 0"),
        ("mach,cd\n0,0.5\n", None, "mass_per_area_kg_m2"),
        (
            "# mass_per_area_kg_m2: 100\nmach,bc_kg_m2\n0,100\n",
            Some(1),
            "only with a cd",
        ),
        (
            "# drag_law: G7\nmach,bc_lb_in2\n0,0.3\n",
            Some(1),
            "drag law \"G7\"",
        ),
    ];
    for (text, line, msg) in cases {
        let e: ImportError = err(text);
        assert_eq!(e.line, line, "{text:?}: {e}");
        assert!(e.to_string().contains(msg), "{text:?}: {e}");
    }
    // The kernel's own checks: Mach strictly increasing, and Mach needed for several rows.
    assert!(
        err("mach,bc_kg_m2\n1,100\n0.5,90\n")
            .message
            .contains("strictly increasing")
    );
    assert!(
        err("bc_kg_m2\n100\n90\n")
            .message
            .contains("needs a mach column")
    );
    assert!(
        err("# mass_per_area_kg_m2: 0\nmach,cd\n0,1\n")
            .message
            .contains("> 0")
    );
}
