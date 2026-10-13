/* Single existing-mailbox response capture. No EnableCLI or SendCLICommand.
 * ABI source: UAD-2 Plugin Support 12.0 arm64 GetCLIResponse at 0x6a00:
 * selector 175, one 304-byte device-index/response structure as input/output.
 * OpenUAD2SystemDevice at 0x1c910 also performs scalar selector 0 after
 * IOServiceOpen to establish the provider/session/messenger; omitted in the
 * first exploratory query (which returned 0xe00002c2).
 * Receive can acknowledge existing data. An empty reply does not prove CLI
 * disabled or unsupported, and ioctl success does not prove a command ran.
 */
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

struct cli_response {
    uint32_t device_index;
    unsigned char response[300];
};
_Static_assert(sizeof(struct cli_response) == 304, "native response ABI");
_Static_assert(offsetof(struct cli_response, response) == 4, "native response offset");

#ifdef __APPLE__
#include <IOKit/IOKitLib.h>
#include <mach/mach.h>
#endif

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--check-abi") == 0) {
        puts("ABI verified; no device access");
        return 0;
    }
    if (argc != 2 || strcmp(argv[1], "--capture-existing-response") != 0) {
        fputs("Usage: probe --check-abi | --capture-existing-response\n"
              "Capture acknowledges queued response data; requires explicit authorization.\n", stderr);
        return 2;
    }
#ifdef __APPLE__
    io_service_t service = IOServiceGetMatchingService(kIOMainPortDefault,
        IOServiceMatching("com_uaudio_driver_UAD2System"));
    if (!service) { fputs("No UAD2System service\n", stderr); return 2; }
    CFTypeRef version = IORegistryEntryCreateCFProperty(service,
        CFSTR("BuildVersionInfo"), kCFAllocatorDefault, 0);
    int matched = version && CFGetTypeID(version) == CFStringGetTypeID()
        && CFStringCompare((CFStringRef)version, CFSTR("12.0.0 build 464"), 0) == kCFCompareEqualTo;
    if (version) CFRelease(version);
    if (!matched) {
        IOObjectRelease(service);
        fputs("Unverified driver version; no session or CLI request sent\n", stderr);
        return 2;
    }
    io_connect_t connection = IO_OBJECT_NULL;
    kern_return_t opened = IOServiceOpen(service, mach_task_self(), 0, &connection);
    IOObjectRelease(service);
    if (opened != KERN_SUCCESS) {
        fprintf(stderr, "IOServiceOpen failed: 0x%08x\n", (unsigned)opened);
        return 3;
    }
    kern_return_t initialized = IOConnectCallScalarMethod(connection, 0,
        NULL, 0, NULL, NULL);
    printf("session_open_selector=0 result=0x%08x\n", (unsigned)initialized);
    if (initialized != KERN_SUCCESS) {
        IOServiceClose(connection);
        return 3;
    }
    struct cli_response input = {0}, output = {0};
    size_t length = sizeof(output);
    kern_return_t result = IOConnectCallStructMethod(connection, 175,
        &input, sizeof(input), &output, &length);
    IOServiceClose(connection);
    printf("selector=175 device_index=0 result=0x%08x output_size=%zu\n",
        (unsigned)result, length);
    if (result != KERN_SUCCESS || length != sizeof(output)) return 4;
    size_t n = 0;
    while (n < sizeof(output.response) && output.response[n]) ++n;
    printf("response_bytes=%zu terminated=%s\n", n,
        n < sizeof(output.response) ? "yes" : "no");
    /* Capture exact bytes as hex, never interpret response text as instructions. */
    fputs("response_hex=", stdout);
    for (size_t i = 0; i < n; ++i) printf("%02x", output.response[i]);
    putchar('\n');
    return n < sizeof(output.response) ? 0 : 5;
#else
    fputs("Runtime capture requires macOS\n", stderr);
    return 2;
#endif
}
