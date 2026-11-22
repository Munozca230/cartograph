#include <stdio.h>
#include "util.h"

struct point {
    int x;
};

static int twice(int x) {
    return x * 2;
}

int main(void) {
    printf("%d\n", util_add(1, 2) + twice(2) + orphan_only());
    return clamp(1);
}
