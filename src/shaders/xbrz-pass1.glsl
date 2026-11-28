// xBRZ Freescale - Pass 1 (corner classification at source resolution)
// Adapted from libretro common-shaders xbrz-freescale-multipass (GPLv3).

#define BLEND_NONE 0
#define BLEND_NORMAL 1
#define BLEND_DOMINANT 2
#define EQUAL_COLOR_TOLERANCE (30.0 / 255.0)
#define STEEP_DIRECTION_THRESHOLD 2.2
#define DOMINANT_DIRECTION_THRESHOLD 3.6
#define S(x, y) src[(x) * 5 + (y)]

float distYCbCr(vec3 a, vec3 b) {
    const vec3 w = vec3(0.2627, 0.6780, 0.0593);
    const float scaleB = 0.5 / (1.0 - w.b);
    const float scaleR = 0.5 / (1.0 - w.r);
    vec3 diff = a - b;
    float y = dot(diff, w);
    float cb = scaleB * (diff.b - y);
    float cr = scaleR * (diff.r - y);
    return sqrt(y * y + cb * cb + cr * cr);
}

bool isPixEqual(vec3 a, vec3 b) {
    return distYCbCr(a, b) < EQUAL_COLOR_TOLERANCE;
}

bool eq(vec3 a, vec3 b) {
    return all(equal(a, b));
}

bool neq(vec3 a, vec3 b) {
    return !eq(a, b);
}

vec3 fetchTap(ivec2 p, ivec2 maxCoord) {
    return texelFetch(inputImage, clamp(p, ivec2(0), maxCoord), 0).rgb;
}

void main() {
    ivec2 maxCoord = textureSize(inputImage, 0) - ivec2(1);
    ivec2 gxy = ivec2(gl_FragCoord.xy);

    vec3 src[25];
    for (int x = 0; x < 5; ++x) {
        for (int y = 0; y < 5; ++y) {
            S(x, y) = fetchTap(gxy + ivec2(x - 2, y - 2), maxCoord);
        }
    }

    vec3 A = S(1, 1);
    vec3 B = S(2, 1);
    vec3 C = S(3, 1);
    vec3 D = S(1, 2);
    vec3 E = S(2, 2);
    vec3 F = S(3, 2);
    vec3 G = S(1, 3);
    vec3 H = S(2, 3);
    vec3 I = S(3, 3);

    ivec4 blendResult = ivec4(BLEND_NONE);

    if (!((eq(E, F) && eq(H, I)) || (eq(E, H) && eq(F, I)))) {
        float distHF = distYCbCr(G, E) + distYCbCr(E, C) + distYCbCr(S(2, 4), I) + distYCbCr(I, S(4, 2)) + 4.0 * distYCbCr(H, F);
        float distEI = distYCbCr(D, H) + distYCbCr(H, S(3, 4)) + distYCbCr(B, F) + distYCbCr(F, S(4, 3)) + 4.0 * distYCbCr(E, I);
        bool dom = DOMINANT_DIRECTION_THRESHOLD * distHF < distEI;
        blendResult.z = (distHF < distEI && neq(E, F) && neq(E, H)) ? (dom ? BLEND_DOMINANT : BLEND_NORMAL) : BLEND_NONE;
    }

    if (!((eq(D, E) && eq(G, H)) || (eq(D, G) && eq(E, H)))) {
        float distGE = distYCbCr(S(0, 3), D) + distYCbCr(D, B) + distYCbCr(S(1, 4), H) + distYCbCr(H, F) + 4.0 * distYCbCr(G, E);
        float distDH = distYCbCr(S(0, 2), G) + distYCbCr(G, S(2, 4)) + distYCbCr(A, E) + distYCbCr(E, I) + 4.0 * distYCbCr(D, H);
        bool dom = DOMINANT_DIRECTION_THRESHOLD * distDH < distGE;
        blendResult.w = (distGE > distDH && neq(E, D) && neq(E, H)) ? (dom ? BLEND_DOMINANT : BLEND_NORMAL) : BLEND_NONE;
    }

    if (!((eq(B, C) && eq(E, F)) || (eq(B, E) && eq(C, F)))) {
        float distEC = distYCbCr(D, B) + distYCbCr(B, S(3, 0)) + distYCbCr(H, F) + distYCbCr(F, S(4, 1)) + 4.0 * distYCbCr(E, C);
        float distBF = distYCbCr(A, E) + distYCbCr(E, I) + distYCbCr(S(2, 0), C) + distYCbCr(C, S(4, 2)) + 4.0 * distYCbCr(B, F);
        bool dom = DOMINANT_DIRECTION_THRESHOLD * distBF < distEC;
        blendResult.y = (distEC > distBF && neq(E, B) && neq(E, F)) ? (dom ? BLEND_DOMINANT : BLEND_NORMAL) : BLEND_NONE;
    }

    if (!((eq(A, B) && eq(D, E)) || (eq(A, D) && eq(B, E)))) {
        float distDB = distYCbCr(S(0, 2), A) + distYCbCr(A, S(2, 0)) + distYCbCr(G, E) + distYCbCr(E, C) + 4.0 * distYCbCr(D, B);
        float distAE = distYCbCr(S(0, 1), D) + distYCbCr(D, H) + distYCbCr(S(1, 0), B) + distYCbCr(B, F) + 4.0 * distYCbCr(A, E);
        bool dom = DOMINANT_DIRECTION_THRESHOLD * distDB < distAE;
        blendResult.x = (distDB < distAE && neq(E, D) && neq(E, B)) ? (dom ? BLEND_DOMINANT : BLEND_NORMAL) : BLEND_NONE;
    }

    vec4 info = vec4(blendResult);

    if (blendResult.z == BLEND_DOMINANT || (blendResult.z == BLEND_NORMAL &&
        !((blendResult.y != BLEND_NONE && !isPixEqual(E, G)) || (blendResult.w != BLEND_NONE && !isPixEqual(E, C)) ||
          (isPixEqual(G, H) && isPixEqual(H, I) && isPixEqual(I, F) && isPixEqual(F, C) && !isPixEqual(E, I))))) {
        info.z += 4.0;
        float distFG = distYCbCr(F, G);
        float distHC = distYCbCr(H, C);
        if (STEEP_DIRECTION_THRESHOLD * distFG <= distHC && neq(E, G) && neq(D, G)) info.z += 16.0;
        if (STEEP_DIRECTION_THRESHOLD * distHC <= distFG && neq(E, C) && neq(B, C)) info.z += 64.0;
    }

    if (blendResult.w == BLEND_DOMINANT || (blendResult.w == BLEND_NORMAL &&
        !((blendResult.z != BLEND_NONE && !isPixEqual(E, A)) || (blendResult.x != BLEND_NONE && !isPixEqual(E, I)) ||
          (isPixEqual(A, D) && isPixEqual(D, G) && isPixEqual(G, H) && isPixEqual(H, I) && !isPixEqual(E, G))))) {
        info.w += 4.0;
        float distHA = distYCbCr(H, A);
        float distDI = distYCbCr(D, I);
        if (STEEP_DIRECTION_THRESHOLD * distHA <= distDI && neq(E, A) && neq(B, A)) info.w += 16.0;
        if (STEEP_DIRECTION_THRESHOLD * distDI <= distHA && neq(E, I) && neq(F, I)) info.w += 64.0;
    }

    if (blendResult.y == BLEND_DOMINANT || (blendResult.y == BLEND_NORMAL &&
        !((blendResult.x != BLEND_NONE && !isPixEqual(E, I)) || (blendResult.z != BLEND_NONE && !isPixEqual(E, A)) ||
          (isPixEqual(I, F) && isPixEqual(F, C) && isPixEqual(C, B) && isPixEqual(B, A) && !isPixEqual(E, C))))) {
        info.y += 4.0;
        float distBI = distYCbCr(B, I);
        float distFA = distYCbCr(F, A);
        if (STEEP_DIRECTION_THRESHOLD * distBI <= distFA && neq(E, I) && neq(H, I)) info.y += 16.0;
        if (STEEP_DIRECTION_THRESHOLD * distFA <= distBI && neq(E, A) && neq(D, A)) info.y += 64.0;
    }

    if (blendResult.x == BLEND_DOMINANT || (blendResult.x == BLEND_NORMAL &&
        !((blendResult.w != BLEND_NONE && !isPixEqual(E, C)) || (blendResult.y != BLEND_NONE && !isPixEqual(E, G)) ||
          (isPixEqual(C, B) && isPixEqual(B, A) && isPixEqual(A, D) && isPixEqual(D, G) && !isPixEqual(E, A))))) {
        info.x += 4.0;
        float distDC = distYCbCr(D, C);
        float distBG = distYCbCr(B, G);
        if (STEEP_DIRECTION_THRESHOLD * distDC <= distBG && neq(E, C) && neq(F, C)) info.x += 16.0;
        if (STEEP_DIRECTION_THRESHOLD * distBG <= distDC && neq(E, G) && neq(H, G)) info.x += 64.0;
    }

    outputColor = info / 255.0;
}
