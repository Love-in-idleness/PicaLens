// CRT-Easymode shader
// Adapted from libretro common-shaders (crt-easymode.cg, GPL).

#define FIX(c) max(abs(c), 1e-5)
#define PI 3.141592653589

vec3 fetchDilated(ivec2 coord, ivec2 maxCoord) {
    vec3 col = texelFetch(inputImage, clamp(coord, ivec2(0), maxCoord), 0).rgb;
    return col * col;
}

float curveDistance(float x, float sharp) {
    float xStep = step(0.5, x);
    float curve = 0.5 - sqrt(max(0.0, 0.25 - (x - xStep) * (x - xStep))) * sign(0.5 - x);
    return mix(x, curve, sharp);
}

vec3 filterLanczosRow(vec4 coeffs, ivec2 base, ivec2 maxCoord) {
    vec3 c0 = fetchDilated(base + ivec2(-1, 0), maxCoord);
    vec3 c1 = fetchDilated(base,                maxCoord);
    vec3 c2 = fetchDilated(base + ivec2( 1, 0), maxCoord);
    vec3 c3 = fetchDilated(base + ivec2( 2, 0), maxCoord);
    vec3 col = c0 * coeffs.x + c1 * coeffs.y + c2 * coeffs.z + c3 * coeffs.w;
    return clamp(col, min(c1, c2), max(c1, c2));
}

void main() {
    ivec2 inputSizeI = textureSize(inputImage, 0);
    ivec2 maxCoord = inputSizeI - ivec2(1);
    vec2 inputSize = vec2(inputSizeI);
    vec2 pos = gl_FragCoord.xy / outputSize;

    const float sharpnessH = 0.5;
    const float sharpnessV = 1.0;
    const float maskStrength = 0.3;
    const float scanlineStrength = 1.0;
    const float scanlineBeamMin = 1.5;
    const float scanlineBeamMax = 1.5;
    const float scanlineBrightMin = 0.35;
    const float scanlineBrightMax = 0.65;
    const float gammaInput = 2.0;
    const float gammaOutput = 1.8;
    const float brightBoost = 1.2;

    vec2 pixCo = pos * inputSize - 0.5;
    ivec2 base = ivec2(floor(pixCo));
    vec2 dist = fract(pixCo);

    float curveX = curveDistance(dist.x, sharpnessH * sharpnessH);
    vec4 coeffs = PI * vec4(1.0 + curveX, curveX, 1.0 - curveX, 2.0 - curveX);
    coeffs = FIX(coeffs);
    coeffs = 2.0 * sin(coeffs) * sin(coeffs * 0.5) / (coeffs * coeffs);
    coeffs /= dot(coeffs, vec4(1.0));

    vec3 col = filterLanczosRow(coeffs, base, maxCoord);
    vec3 col2 = filterLanczosRow(coeffs, base + ivec2(0, 1), maxCoord);
    col = mix(col, col2, curveDistance(dist.y, sharpnessV));
    col = pow(max(col, vec3(0.0)), vec3(gammaInput * 0.5));

    float luma = dot(vec3(0.2126, 0.7152, 0.0722), col);
    float bright = (max(col.r, max(col.g, col.b)) + luma) * 0.5;
    float scanBright = clamp(bright, scanlineBrightMin, scanlineBrightMax);
    float scanBeam = clamp(bright * scanlineBeamMax, scanlineBeamMin, scanlineBeamMax);
    float scanWeight = inputSizeI.y >= 400
        ? 1.0
        : 1.0 - pow(cos(pos.y * 2.0 * PI * inputSize.y) * 0.5 + 0.5, scanBeam) * scanlineStrength;

    float mask = 1.0 - maskStrength;
    int dotNo = int(mod(floor(gl_FragCoord.x), 3.0));
    vec3 maskWeight = dotNo == 0
        ? vec3(1.0, mask, mask)
        : (dotNo == 1 ? vec3(mask, 1.0, mask) : vec3(mask, mask, 1.0));

    col = mix(col * scanWeight, col, scanBright);
    col *= maskWeight;
    col = pow(max(col, vec3(0.0)), vec3(1.0 / gammaOutput));
    outputColor = vec4(clamp(col * brightBoost, 0.0, 1.0), 1.0);
}
