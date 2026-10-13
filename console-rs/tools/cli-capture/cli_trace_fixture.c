/* Device-free DTrace fixture. No IOKit imports or device/session access. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

__attribute__((noinline, visibility("default")))
int IOConnectCallStructMethod(uint32_t connection, uint32_t selector,
        const void *input, size_t input_size, void *output, size_t *output_size) {
    (void)connection; (void)input; (void)input_size;
    if (selector == 175 && output && output_size && *output_size == 304) {
        memset(output, 0, 304);
        memcpy((unsigned char *)output + 4, "fixture-response", 16);
        return 0;
    }
    return selector == 175 ? (int)0xe00002c2u : 0;
}

int main(void) {
    uint32_t enable[2] = {0, 1};
    unsigned char command[132] = {0}, response[304] = {0};
    size_t length = sizeof(response);
    memcpy(command + 4, "fixture-only-no-device", 22);
    sleep(2);
    volatile int result = IOConnectCallStructMethod(42, 173, enable, 8, NULL, NULL);
    result = IOConnectCallStructMethod(42, 174, command, 132, NULL, NULL);
    result = IOConnectCallStructMethod(42, 175, response, 304, response, &length);
    result = IOConnectCallStructMethod(42, 175, response, 304, NULL, NULL);
    (void)result;
    puts("fixture complete; no device access");
    sleep(12);
    return 0;
}
