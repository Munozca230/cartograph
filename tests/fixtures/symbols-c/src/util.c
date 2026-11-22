#include "util.h"

static int clamp(int x) {
    return x < 0 ? 0 : x;
}

int util_add(int a, int b) {
    return clamp(a + b);
}
