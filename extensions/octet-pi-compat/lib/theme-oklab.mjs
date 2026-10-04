// Pure color helpers adapted from Pi 1.0 581e7ba78141a4d8b61cc9d11b8b22ae7e59195e.
// packages/tui/src/oklab.ts sha256 45b067e6e3605b385f595adecd7c0216f1c6b6686680d5c73f661286de736be6
// Copyright (c) 2025 Mario Zechner. MIT, see ../LICENSE.pi.
const multiply = (m, [x, y, z])=>m.map((row)=>row[0] * x + row[1] * y + row[2] * z);
const LINEAR_SRGB_TO_LMS = [
    [
        0.4122214694707629,
        0.5363325372617349,
        0.0514459932675022
    ],
    [
        0.2119034958178251,
        0.6806995506452344,
        0.1073969535369405
    ],
    [
        0.0883024591900564,
        0.2817188391361215,
        0.6299787016738222
    ]
];
const LMS_TO_LAB = [
    [
        0.210454268309314,
        0.793617774702305,
        -0.0040720430116193
    ],
    [
        1.9779985324311684,
        -2.42859224204858,
        0.450593709617411
    ],
    [
        0.0259040424655478,
        0.7827717124575296,
        -0.8086757549230774
    ]
];
const LAB_TO_LMS = [
    [
        1,
        0.3963377773761749,
        0.2158037573099136
    ],
    [
        1,
        -0.1055613458156586,
        -0.0638541728258133
    ],
    [
        1,
        -0.0894841775298119,
        -1.2914855480194092
    ]
];
const LMS_TO_LINEAR_SRGB = [
    [
        4.0767416360759583,
        -3.3077115392580629,
        0.2309699031821043
    ],
    [
        -1.2684379732850315,
        2.6097573492876882,
        -0.341319376002657
    ],
    [
        -0.0041960761386756,
        -0.7034186179359362,
        1.7076146940746117
    ]
];
const SATURATION_FIT = [
    [
        [
            -1.8817031,
            -0.80936501
        ],
        [
            1.19086277,
            1.76576728,
            0.59662641,
            0.75515197,
            0.56771245
        ]
    ],
    [
        [
            1.8144408,
            -1.19445267
        ],
        [
            0.73956515,
            -0.45954404,
            0.08285427,
            0.12541073,
            -0.14503204
        ]
    ],
    [
        [
            0.13110758,
            1.81333971
        ],
        [
            1.35733652,
            -0.00915799,
            -1.1513021,
            -0.50559606,
            0.00692167
        ]
    ]
];
const K1 = 0.206;
const K2 = 0.03;
const K3 = (1 + K1) / (1 + K2);
export const oklabToOkhslLightness = (x)=>0.5 * (K3 * x - K1 + Math.sqrt((K3 * x - K1) ** 2 + 4 * K2 * K3 * x));
const okhslToOklabLightness = (x)=>(x * x + K1 * x) / (K3 * (x + K2));
const linearToSrgb = (value)=>value > 0.0031308 ? 1.055 * value ** (1 / 2.4) - 0.055 : 12.92 * value;
const srgbToLinear = (value)=>value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
export function oklabToLinearSrgb(lab) {
    return multiply(LMS_TO_LINEAR_SRGB, multiply(LAB_TO_LMS, lab).map((value)=>value ** 3));
}
function linearSrgbToOklab(rgb) {
    return multiply(LMS_TO_LAB, multiply(LINEAR_SRGB_TO_LMS, rgb).map(Math.cbrt));
}
export function rgbToOklab({ r, g, b }) {
    return linearSrgbToOklab([
        r / 255,
        g / 255,
        b / 255
    ].map(srgbToLinear));
}
export function linearSrgbToRgb(linear) {
    const [r, g, b] = linear.map((value)=>Math.round(Math.min(1, Math.max(0, linearToSrgb(value))) * 255));
    return {
        r,
        g,
        b
    };
}
function lmsSlopes(a, b) {
    return [
        LAB_TO_LMS[0],
        LAB_TO_LMS[1],
        LAB_TO_LMS[2]
    ].map((row)=>row[1] * a + row[2] * b);
}
function maxSaturation(a, b) {
    const channel = SATURATION_FIT.findIndex(([[x, y]], index)=>index === 2 || x * a + y * b > 1);
    const [k0, k1, k2, k3, k4] = SATURATION_FIT[channel][1];
    const weights = LMS_TO_LINEAR_SRGB[channel];
    const saturation = k0 + k1 * a + k2 * b + k3 * a * a + k4 * a * b;
    const slopes = lmsSlopes(a, b);
    const base = slopes.map((k)=>1 + saturation * k);
    const dot = (values)=>values.reduce((sum, value, index)=>sum + weights[index] * value, 0);
    const f = dot(base.map((value)=>value ** 3));
    const f1 = dot(base.map((value, index)=>3 * slopes[index] * value ** 2));
    const f2 = dot(base.map((value, index)=>6 * slopes[index] ** 2 * value));
    return saturation - f * f1 / (f1 * f1 - 0.5 * f * f2);
}
function cusp(a, b) {
    const saturation = maxSaturation(a, b);
    const lightness = Math.cbrt(1 / Math.max(...oklabToLinearSrgb([
        1,
        saturation * a,
        saturation * b
    ])));
    return [
        lightness,
        lightness * saturation
    ];
}
function maxChroma(a, b, lightness, [cuspL, cuspC]) {
    if (lightness <= cuspL) return cuspC * lightness / cuspL;
    const t = cuspC * (lightness - 1) / (cuspL - 1);
    const slopes = lmsSlopes(a, b);
    const lms = slopes.map((k)=>lightness + t * k);
    const cubes = lms.map((value)=>value ** 3);
    const first = lms.map((value, index)=>3 * slopes[index] * value ** 2);
    const second = lms.map((value, index)=>6 * slopes[index] ** 2 * value);
    const dot = (row, values)=>row[0] * values[0] + row[1] * values[1] + row[2] * values[2];
    const steps = LMS_TO_LINEAR_SRGB.map((row)=>{
        const f = dot(row, cubes) - 1;
        const f1 = dot(row, first);
        const f2 = dot(row, second);
        const u = f1 / (f1 * f1 - 0.5 * f * f2);
        return u >= 0 ? -f * u : Number.MAX_VALUE;
    });
    return t + Math.min(...steps);
}
function chromaStops(L, a, b) {
    const peak = cusp(a, b);
    const cMax = maxChroma(a, b, L, peak);
    const k = cMax / Math.min(L * (peak[1] / peak[0]), (1 - L) * (peak[1] / (1 - peak[0])));
    const midS = 0.11516993 + 1 / (7.4477897 + 4.1590124 * b + a * (-2.19557347 + 1.75198401 * b + a * (-2.13704948 - 10.02301043 * b + a * (-4.24894561 + 5.38770819 * b + 4.69891013 * a))));
    const midT = 0.11239642 + 1 / (1.6132032 - 0.68124379 * b + a * (0.40370612 + 0.90148123 * b + a * (-0.27087943 + 0.6122399 * b + a * (0.00299215 - 0.45399568 * b - 0.14661872 * a))));
    const cMid = 0.9 * k * Math.sqrt(Math.sqrt(1 / (1 / (L * midS) ** 4 + 1 / ((1 - L) * midT) ** 4)));
    const c0 = Math.sqrt(1 / (1 / (L * 0.4) ** 2 + 1 / ((1 - L) * 0.8) ** 2));
    return [
        c0,
        cMid,
        cMax
    ];
}
export function okhslToRgb(hue, saturation, lightness) {
    const L = okhslToOklabLightness(lightness);
    let lab = [
        L,
        0,
        0
    ];
    if (L > 0 && L < 1 && saturation > 0) {
        const angle = 2 * Math.PI * ((hue % 360 + 360) % 360) / 360;
        const a = Math.cos(angle);
        const b = Math.sin(angle);
        const [c0, cMid, cMax] = chromaStops(L, a, b);
        let chroma;
        if (saturation < 0.8) {
            const t = 1.25 * saturation;
            const k1 = 0.8 * c0;
            chroma = t * k1 / (1 - (1 - k1 / cMid) * t);
        } else {
            const t = 5 * (saturation - 0.8);
            const k1 = 0.2 * cMid ** 2 * 1.25 ** 2 / c0;
            chroma = cMid + t * k1 / (1 - (1 - k1 / (cMax - cMid)) * t);
        }
        lab = [
            L,
            chroma * a,
            chroma * b
        ];
    }
    return linearSrgbToRgb(oklabToLinearSrgb(lab));
}
export function rgbToOkhsl(rgb) {
    const [L, labA, labB] = rgbToOklab(rgb);
    const chroma = Math.hypot(labA, labB);
    const lightness = oklabToOkhslLightness(L);
    if (chroma < 1e-9 || lightness <= 0 || lightness >= 1) return {
        h: 0,
        s: 0,
        l: lightness
    };
    const hue = (Math.atan2(labB, labA) * 180 / Math.PI + 360) % 360;
    const [c0, cMid, cMax] = chromaStops(L, labA / chroma, labB / chroma);
    let saturation;
    if (chroma < cMid) {
        const k1 = 0.8 * c0;
        saturation = 0.8 * (chroma / (k1 + (1 - k1 / cMid) * chroma));
    } else {
        const k1 = 0.2 * cMid ** 2 * 1.25 ** 2 / c0;
        const offset = chroma - cMid;
        saturation = 0.8 + 0.2 * (offset / (k1 + (1 - k1 / (cMax - cMid)) * offset));
    }
    return {
        h: hue,
        s: Math.min(1, Math.max(0, saturation)),
        l: lightness
    };
}
