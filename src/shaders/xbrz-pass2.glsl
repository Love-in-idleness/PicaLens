// xBRZ Freescale - Pass 2 (arbitrary scale reconstruction)
// Adapted from libretro common-shaders xbrz-freescale-multipass (GPLv3).

uniform sampler2D xbrzInfo;

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

float getLeftRatio(vec2 center, vec2 origin, vec2 direction, vec2 scale) {
    vec2 p0 = center - origin;
    vec2 proj = direction * (dot(p0, direction) / dot(direction, direction));
    vec2 distv = p0 - proj;
    vec2 orth = vec2(-direction.y, direction.x);
    float side = sign(dot(p0, orth));
    float v = side * length(distv * scale);
    return smoothstep(-sqrt(2.0) * 0.5, sqrt(2.0) * 0.5, v);
}

vec3 fetchTap(ivec2 base, ivec2 off, ivec2 maxCoord) {
    return texelFetch(inputImage, clamp(base + off, ivec2(0), maxCoord), 0).rgb;
}

void main() {
    ivec2 inputSizeI = textureSize(inputImage, 0);
    ivec2 maxCoord = inputSizeI - ivec2(1);
    vec2 inputSize = vec2(inputSizeI);
    vec2 pos = gl_FragCoord.xy / outputSize;
    vec2 scale = outputSize / inputSize;
    vec2 srcPos = pos * inputSize;
    ivec2 base = clamp(ivec2(floor(srcPos)), ivec2(0), maxCoord);
    vec2 f = fract(srcPos) - 0.5;

    vec3 B = fetchTap(base, ivec2( 0, -1), maxCoord);
    vec3 D = fetchTap(base, ivec2(-1,  0), maxCoord);
    vec3 E = fetchTap(base, ivec2( 0,  0), maxCoord);
    vec3 F = fetchTap(base, ivec2( 1,  0), maxCoord);
    vec3 H = fetchTap(base, ivec2( 0,  1), maxCoord);

    vec4 info = floor(texelFetch(xbrzInfo, base, 0) * 255.0 + 0.5);
    vec4 blendResult = floor(mod(info, 4.0));
    vec4 doLineBlend = floor(mod(info / 4.0, 4.0));
    vec4 haveShallowLine = floor(mod(info / 16.0, 4.0));
    vec4 haveSteepLine = floor(mod(info / 64.0, 4.0));

    vec3 res = E;

    if (blendResult.z > 0.0) {
        vec2 origin = vec2(0.0, 1.0 / sqrt(2.0));
        vec2 direction = vec2(1.0, -1.0);
        if (doLineBlend.z > 0.0) {
            origin = haveShallowLine.z > 0.0 ? vec2(0.0, 0.25) : vec2(0.0, 0.5);
            direction.x += haveShallowLine.z;
            direction.y -= haveSteepLine.z;
        }
        vec3 blendPix = mix(H, F, step(distYCbCr(E, F), distYCbCr(E, H)));
        res = mix(res, blendPix, getLeftRatio(f, origin, direction, scale));
    }

    if (blendResult.w > 0.0) {
        vec2 origin = vec2(-1.0 / sqrt(2.0), 0.0);
        vec2 direction = vec2(1.0, 1.0);
        if (doLineBlend.w > 0.0) {
            origin = haveShallowLine.w > 0.0 ? vec2(-0.25, 0.0) : vec2(-0.5, 0.0);
            direction.y += haveShallowLine.w;
            direction.x += haveSteepLine.w;
        }
        vec3 blendPix = mix(H, D, step(distYCbCr(E, D), distYCbCr(E, H)));
        res = mix(res, blendPix, getLeftRatio(f, origin, direction, scale));
    }

    if (blendResult.y > 0.0) {
        vec2 origin = vec2(1.0 / sqrt(2.0), 0.0);
        vec2 direction = vec2(-1.0, -1.0);
        if (doLineBlend.y > 0.0) {
            origin = haveShallowLine.y > 0.0 ? vec2(0.25, 0.0) : vec2(0.5, 0.0);
            direction.y -= haveShallowLine.y;
            direction.x -= haveSteepLine.y;
        }
        vec3 blendPix = mix(F, B, step(distYCbCr(E, B), distYCbCr(E, F)));
        res = mix(res, blendPix, getLeftRatio(f, origin, direction, scale));
    }

    if (blendResult.x > 0.0) {
        vec2 origin = vec2(0.0, -1.0 / sqrt(2.0));
        vec2 direction = vec2(-1.0, 1.0);
        if (doLineBlend.x > 0.0) {
            origin = haveShallowLine.x > 0.0 ? vec2(0.0, -0.25) : vec2(0.0, -0.5);
            direction.x -= haveShallowLine.x;
            direction.y += haveSteepLine.x;
        }
        vec3 blendPix = mix(D, B, step(distYCbCr(E, B), distYCbCr(E, D)));
        res = mix(res, blendPix, getLeftRatio(f, origin, direction, scale));
    }

    outputColor = vec4(res, 1.0);
}
