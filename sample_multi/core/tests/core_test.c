#include <stdio.h>
#include <string.h>

#include "core.h"

int main(void) {
    if (frost_core_add(40, 2) != 42) {
        printf("core_test: frost_core_add is wrong\n");
        return 1;
    }
    if (strcmp(frost_core_version(), "1") != 0) {
        printf("core_test: unexpected version %s\n", frost_core_version());
        return 1;
    }
    printf("core_test: ok\n");
    return 0;
}
