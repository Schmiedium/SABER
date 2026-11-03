//! Butcher tableaux for the explicit Runge–Kutta methods (D14, `physics-engine-tradeoffs.md` §10.1).
//!
//! Every coefficient is written once, as the exact value from its source (a ratio of integers,
//! or for DOP853 the 30-digit published decimal, since its nodes involve √6). The `f64` used by the
//! stepper is derived from that exact value by the compiler, which rounds correctly. The tests in
//! `tests/tableau_order_conditions.rs` check the order conditions in exact rational arithmetic and
//! that each `f64` is the correctly rounded exact value (`verification-and-test-strategy.md` §5.4.1).

use sim_schema::IntegratorKind;

/// The exact value a coefficient was rounded from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exact {
    /// `numerator / denominator`.
    Ratio(i64, i64),
    /// A decimal literal as published (DOP853).
    Decimal(&'static str),
}

/// One Butcher coefficient: the `f64` the stepper uses and the exact value it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coef {
    /// Correctly rounded value.
    pub value: f64,
    /// Exact source value.
    pub exact: Exact,
}

impl Coef {
    /// The coefficient 0.
    pub const ZERO: Coef = Coef {
        value: 0.0,
        exact: Exact::Ratio(0, 1),
    };
}

/// How a method estimates its local error.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ErrorWeights<const S: usize> {
    /// No embedded solution: no error estimate, fixed steps only.
    None,
    /// Weights `b̂` of the embedded solution; the estimate is `h·Σ (bᵢ − b̂ᵢ) kᵢ`.
    Embedded([Coef; S]),
    /// DOP853's composite estimator (Hairer, Nørsett and Wanner, *Solving ODEs I*, §II.10):
    /// `e5 = b − b̂₅` (5th-order embedded) and `b̂₃` (3rd-order embedded), combined as
    /// `|h|·‖e5‖² / sqrt(n·(‖e5‖² + 0.01·‖b − b̂₃‖²))`, which scales as `h⁸`.
    Dop853 {
        /// `b − b̂₅`.
        e5: [Coef; S],
        /// Weights of the 3rd-order embedded solution.
        b_hat3: [Coef; S],
    },
}

/// An explicit Runge–Kutta method with `S` stages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tableau<const S: usize> {
    /// Which method this is.
    pub kind: IntegratorKind,
    /// Stage matrix, strictly lower triangular.
    pub a: [[Coef; S]; S],
    /// Weights of the propagated solution.
    pub b: [Coef; S],
    /// Nodes, `cᵢ = Σⱼ aᵢⱼ`.
    pub c: [Coef; S],
    /// Embedded error estimate, if any.
    pub error: ErrorWeights<S>,
    /// Order of the propagated solution.
    pub order: u8,
    /// Order of the embedded solution (0 if none; the lower one for DOP853).
    pub embedded_order: u8,
    /// The error norm scales as `h^(err_order+1)`; sets the step controller's exponent.
    pub err_order: u8,
    /// First same as last: the last stage is `f(t + h, y_{n+1})`, reused as the next step's first
    /// stage within a tick.
    pub fsal: bool,
}

/// Exact ratio `n/d` (or an integer `n`) as a coefficient.
macro_rules! q {
    ($n:literal / $d:literal) => {
        Coef {
            value: ($n as f64) / ($d as f64),
            exact: Exact::Ratio($n, $d),
        }
    };
    ($n:literal) => {
        Coef {
            value: $n as f64,
            exact: Exact::Ratio($n, 1),
        }
    };
}

/// A published decimal as a coefficient; the compiler rounds the literal correctly.
macro_rules! d {
    ($x:literal) => {
        Coef {
            value: $x,
            exact: Exact::Decimal(stringify!($x)),
        }
    };
}

const Z: Coef = Coef::ZERO;

/// Explicit midpoint, order 2.
pub const RK2_MIDPOINT: Tableau<2> = Tableau {
    kind: IntegratorKind::Rk2Midpoint,
    a: [[Z, Z], [q!(1 / 2), Z]],
    b: [Z, q!(1)],
    c: [Z, q!(1 / 2)],
    error: ErrorWeights::None,
    order: 2,
    embedded_order: 0,
    err_order: 0,
    fsal: false,
};

/// Classic Runge–Kutta, order 4.
pub const RK4: Tableau<4> = Tableau {
    kind: IntegratorKind::Rk4,
    a: [
        [Z, Z, Z, Z],
        [q!(1 / 2), Z, Z, Z],
        [Z, q!(1 / 2), Z, Z],
        [Z, Z, q!(1), Z],
    ],
    b: [q!(1 / 6), q!(1 / 3), q!(1 / 3), q!(1 / 6)],
    c: [Z, q!(1 / 2), q!(1 / 2), q!(1)],
    error: ErrorWeights::None,
    order: 4,
    embedded_order: 0,
    err_order: 0,
    fsal: false,
};

/// Bogacki–Shampine 3(2), FSAL (Bogacki and Shampine, Appl. Math. Lett. 2(4), 1989).
pub const BOGACKI_SHAMPINE_32: Tableau<4> = Tableau {
    kind: IntegratorKind::BogackiShampine32,
    a: [
        [Z, Z, Z, Z],
        [q!(1 / 2), Z, Z, Z],
        [Z, q!(3 / 4), Z, Z],
        [q!(2 / 9), q!(1 / 3), q!(4 / 9), Z],
    ],
    b: [q!(2 / 9), q!(1 / 3), q!(4 / 9), Z],
    c: [Z, q!(1 / 2), q!(3 / 4), q!(1)],
    error: ErrorWeights::Embedded([q!(7 / 24), q!(1 / 4), q!(1 / 3), q!(1 / 8)]),
    order: 3,
    embedded_order: 2,
    err_order: 2,
    fsal: true,
};

/// Runge–Kutta–Fehlberg 4(5), propagating the 4th-order solution (Fehlberg, NASA TR R-315, 1969).
pub const FEHLBERG_45: Tableau<6> = Tableau {
    kind: IntegratorKind::Fehlberg45,
    a: [
        [Z, Z, Z, Z, Z, Z],
        [q!(1 / 4), Z, Z, Z, Z, Z],
        [q!(3 / 32), q!(9 / 32), Z, Z, Z, Z],
        [q!(1932 / 2197), q!(-7200 / 2197), q!(7296 / 2197), Z, Z, Z],
        [q!(439 / 216), q!(-8), q!(3680 / 513), q!(-845 / 4104), Z, Z],
        [
            q!(-8 / 27),
            q!(2),
            q!(-3544 / 2565),
            q!(1859 / 4104),
            q!(-11 / 40),
            Z,
        ],
    ],
    b: [
        q!(25 / 216),
        Z,
        q!(1408 / 2565),
        q!(2197 / 4104),
        q!(-1 / 5),
        Z,
    ],
    c: [Z, q!(1 / 4), q!(3 / 8), q!(12 / 13), q!(1), q!(1 / 2)],
    error: ErrorWeights::Embedded([
        q!(16 / 135),
        Z,
        q!(6656 / 12825),
        q!(28561 / 56430),
        q!(-9 / 50),
        q!(2 / 55),
    ]),
    order: 4,
    embedded_order: 5,
    err_order: 4,
    fsal: false,
};

/// Cash–Karp 5(4), propagating the 5th-order solution (Cash and Karp, ACM TOMS 16(3), 1990).
pub const CASH_KARP_45: Tableau<6> = Tableau {
    kind: IntegratorKind::CashKarp45,
    a: [
        [Z, Z, Z, Z, Z, Z],
        [q!(1 / 5), Z, Z, Z, Z, Z],
        [q!(3 / 40), q!(9 / 40), Z, Z, Z, Z],
        [q!(3 / 10), q!(-9 / 10), q!(6 / 5), Z, Z, Z],
        [q!(-11 / 54), q!(5 / 2), q!(-70 / 27), q!(35 / 27), Z, Z],
        [
            q!(1631 / 55296),
            q!(175 / 512),
            q!(575 / 13824),
            q!(44275 / 110592),
            q!(253 / 4096),
            Z,
        ],
    ],
    b: [
        q!(37 / 378),
        Z,
        q!(250 / 621),
        q!(125 / 594),
        Z,
        q!(512 / 1771),
    ],
    c: [Z, q!(1 / 5), q!(3 / 10), q!(3 / 5), q!(1), q!(7 / 8)],
    error: ErrorWeights::Embedded([
        q!(2825 / 27648),
        Z,
        q!(18575 / 48384),
        q!(13525 / 55296),
        q!(277 / 14336),
        q!(1 / 4),
    ]),
    order: 5,
    embedded_order: 4,
    err_order: 4,
    fsal: false,
};

/// Dormand–Prince 5(4) (DOPRI5), FSAL (Dormand and Prince, J. Comput. Appl. Math. 6(1), 1980).
pub const DORMAND_PRINCE_54: Tableau<7> = Tableau {
    kind: IntegratorKind::DormandPrince54,
    a: [
        [Z, Z, Z, Z, Z, Z, Z],
        [q!(1 / 5), Z, Z, Z, Z, Z, Z],
        [q!(3 / 40), q!(9 / 40), Z, Z, Z, Z, Z],
        [q!(44 / 45), q!(-56 / 15), q!(32 / 9), Z, Z, Z, Z],
        [
            q!(19372 / 6561),
            q!(-25360 / 2187),
            q!(64448 / 6561),
            q!(-212 / 729),
            Z,
            Z,
            Z,
        ],
        [
            q!(9017 / 3168),
            q!(-355 / 33),
            q!(46732 / 5247),
            q!(49 / 176),
            q!(-5103 / 18656),
            Z,
            Z,
        ],
        [
            q!(35 / 384),
            Z,
            q!(500 / 1113),
            q!(125 / 192),
            q!(-2187 / 6784),
            q!(11 / 84),
            Z,
        ],
    ],
    b: [
        q!(35 / 384),
        Z,
        q!(500 / 1113),
        q!(125 / 192),
        q!(-2187 / 6784),
        q!(11 / 84),
        Z,
    ],
    c: [Z, q!(1 / 5), q!(3 / 10), q!(4 / 5), q!(8 / 9), q!(1), q!(1)],
    error: ErrorWeights::Embedded([
        q!(5179 / 57600),
        Z,
        q!(7571 / 16695),
        q!(393 / 640),
        q!(-92097 / 339200),
        q!(187 / 2100),
        q!(1 / 40),
    ]),
    order: 5,
    embedded_order: 4,
    err_order: 4,
    fsal: true,
};

/// Dormand–Prince 8(5,3) (DOP853), as in Hairer's `dop853.f` (Hairer, Nørsett and Wanner,
/// *Solving Ordinary Differential Equations I*, 2nd ed., §II.10). Its nodes involve √6, so the
/// coefficients are the published 30-digit decimals.
#[allow(clippy::excessive_precision)]
pub const DORMAND_PRINCE_853: Tableau<12> = Tableau {
    kind: IntegratorKind::DormandPrince853,
    a: [
        [Z, Z, Z, Z, Z, Z, Z, Z, Z, Z, Z, Z],
        [
            d!(5.26001519587677318785587544488e-2),
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(1.97250569845378994544595329183e-2),
            d!(5.91751709536136983633785987549e-2),
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(2.95875854768068491816892993775e-2),
            Z,
            d!(8.87627564304205475450678981324e-2),
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(2.41365134159266685502369798665e-1),
            Z,
            d!(-8.84549479328286085344864962717e-1),
            d!(9.24834003261792003115737966543e-1),
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(3.7037037037037037037037037037e-2),
            Z,
            Z,
            d!(1.70828608729473871279604482173e-1),
            d!(1.25467687566822425016691814123e-1),
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(3.7109375e-2),
            Z,
            Z,
            d!(1.70252211019544039314978060272e-1),
            d!(6.02165389804559606850219397283e-2),
            d!(-1.7578125e-2),
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(3.70920001185047927108779319836e-2),
            Z,
            Z,
            d!(1.70383925712239993810214054705e-1),
            d!(1.07262030446373284651809199168e-1),
            d!(-1.53194377486244017527936158236e-2),
            d!(8.27378916381402288758473766002e-3),
            Z,
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(6.24110958716075717114429577812e-1),
            Z,
            Z,
            d!(-3.36089262944694129406857109825),
            d!(-8.68219346841726006818189891453e-1),
            d!(2.75920996994467083049415600797e1),
            d!(2.01540675504778934086186788979e1),
            d!(-4.34898841810699588477366255144e1),
            Z,
            Z,
            Z,
            Z,
        ],
        [
            d!(4.77662536438264365890433908527e-1),
            Z,
            Z,
            d!(-2.48811461997166764192642586468),
            d!(-5.90290826836842996371446475743e-1),
            d!(2.12300514481811942347288949897e1),
            d!(1.52792336328824235832596922938e1),
            d!(-3.32882109689848629194453265587e1),
            d!(-2.03312017085086261358222928593e-2),
            Z,
            Z,
            Z,
        ],
        [
            d!(-9.3714243008598732571704021658e-1),
            Z,
            Z,
            d!(5.18637242884406370830023853209),
            d!(1.09143734899672957818500254654),
            d!(-8.14978701074692612513997267357),
            d!(-1.85200656599969598641566180701e1),
            d!(2.27394870993505042818970056734e1),
            d!(2.49360555267965238987089396762),
            d!(-3.0467644718982195003823669022),
            Z,
            Z,
        ],
        [
            d!(2.27331014751653820792359768449),
            Z,
            Z,
            d!(-1.05344954667372501984066689879e1),
            d!(-2.00087205822486249909675718444),
            d!(-1.79589318631187989172765950534e1),
            d!(2.79488845294199600508499808837e1),
            d!(-2.85899827713502369474065508674),
            d!(-8.87285693353062954433549289258),
            d!(1.23605671757943030647266201528e1),
            d!(6.43392746015763530355970484046e-1),
            Z,
        ],
    ],
    b: [
        d!(5.42937341165687622380535766363e-2),
        Z,
        Z,
        Z,
        Z,
        d!(4.45031289275240888144113950566),
        d!(1.89151789931450038304281599044),
        d!(-5.8012039600105847814672114227),
        d!(3.1116436695781989440891606237e-1),
        d!(-1.52160949662516078556178806805e-1),
        d!(2.01365400804030348374776537501e-1),
        d!(4.47106157277725905176885569043e-2),
    ],
    c: [
        Z,
        d!(0.526001519587677318785587544488e-1),
        d!(0.789002279381515978178381316732e-1),
        d!(0.118350341907227396726757197510),
        d!(0.281649658092772603273242802490),
        d!(0.333333333333333333333333333333),
        d!(0.25),
        d!(0.307692307692307692307692307692),
        d!(0.651282051282051282051282051282),
        d!(0.6),
        d!(0.857142857142857142857142857142),
        d!(1.0),
    ],
    error: ErrorWeights::Dop853 {
        e5: [
            d!(0.1312004499419488073250102996e-1),
            Z,
            Z,
            Z,
            Z,
            d!(-0.1225156446376204440720569753e+1),
            d!(-0.4957589496572501915214079952),
            d!(0.1664377182454986536961530415e+1),
            d!(-0.3503288487499736816886487290),
            d!(0.3341791187130174790297318841),
            d!(0.8192320648511571246570742613e-1),
            d!(-0.2235530786388629525884427845e-1),
        ],
        b_hat3: [
            d!(0.244094488188976377952755905512),
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            Z,
            d!(0.733846688281611857341361741547),
            Z,
            Z,
            d!(0.220588235294117647058823529412e-1),
        ],
    },
    order: 8,
    embedded_order: 5,
    err_order: 7,
    fsal: false,
};

/// Plain `f64` arrays of a tableau, as the stepper uses them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Coeffs<const S: usize> {
    pub a: [[f64; S]; S],
    pub b: [f64; S],
    pub c: [f64; S],
    /// Error weights `b − b̂` (or `e5` for DOP853).
    pub e: [f64; S],
    /// DOP853 only: `b − b̂₃`.
    pub e3: [f64; S],
}

impl<const S: usize> Tableau<S> {
    pub(crate) fn coeffs(&self) -> Coeffs<S> {
        let mut out = Coeffs {
            a: [[0.0; S]; S],
            b: [0.0; S],
            c: [0.0; S],
            e: [0.0; S],
            e3: [0.0; S],
        };
        for i in 0..S {
            for j in 0..S {
                out.a[i][j] = self.a[i][j].value;
            }
            out.b[i] = self.b[i].value;
            out.c[i] = self.c[i].value;
        }
        match &self.error {
            ErrorWeights::None => {}
            ErrorWeights::Embedded(b_hat) => {
                for (e, (b, bh)) in out.e.iter_mut().zip(self.b.iter().zip(b_hat)) {
                    *e = b.value - bh.value;
                }
            }
            ErrorWeights::Dop853 { e5, b_hat3 } => {
                for i in 0..S {
                    out.e[i] = e5[i].value;
                    out.e3[i] = self.b[i].value - b_hat3[i].value;
                }
            }
        }
        out
    }
}
