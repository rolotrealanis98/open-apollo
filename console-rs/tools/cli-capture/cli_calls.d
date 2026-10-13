/* Passive selected-process capture; issues no device calls.
 * ABI: UAD Plugin Support 12.0 arm64 selectors 173/174/175.
 * Fixed-size hex dumps avoid unbounded strings. Return 0 is not MCU success.
 */
#pragma D option bufsize=1m

dtrace:::BEGIN
{
    enables = 0; sends = 0; receives = 0; returns = 0; faults = 0;
    printf("TRACE_READY target=%d duration_seconds=10 abi=ua12-arm64\n", $target);
}

pid$target::IOConnectCallStructMethod:entry
/arg1 >= 173 && arg1 <= 175/
{
    self->selector = arg1;
    self->connection = arg0;
    self->output = arg4;
    self->output_size = arg5;
    self->capacity = arg5 != 0 ? *(uint64_t *)copyin(arg5, 8) : 0;
    printf("CALL time=%llu pid=%d tid=%d connection=%u selector=%u input_size=%llu output_capacity=%llu\n",
        timestamp, pid, tid, (uint32_t)arg0, (uint32_t)arg1, (uint64_t)arg3, (uint64_t)self->capacity);
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 173 && arg3 == 8 && arg2 != 0/
{
    enables++;
    printf("ENABLE index=%u enabled=%d\n",
        *(uint32_t *)copyin(arg2, 4), *(int32_t *)copyin(arg2 + 4, 4));
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 174 && arg3 == 132 && arg2 != 0/
{
    sends++;
    printf("SEND index=%u packet_hex_bytes=132\n", *(uint32_t *)copyin(arg2, 4));
    tracemem(copyin(arg2, 132), 132);
    ustack(12);
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 175 && arg3 == 304 && arg2 != 0/
{
    receives++;
    printf("GET index=%u\n", *(uint32_t *)copyin(arg2, 4));
}

pid$target::IOConnectCallStructMethod:return
/self->selector == 175 && arg1 == 0 && self->output != 0 && self->output_size != 0 && self->capacity >= 304/
{
    self->actual_size = *(uint64_t *)copyin(self->output_size, 8);
}

pid$target::IOConnectCallStructMethod:return
/self->selector == 175 && arg1 == 0 && self->actual_size == 304/
{
    printf("RECEIVE packet_hex_bytes=304\n");
    tracemem(copyin(self->output, 304), 304);
}

pid$target::IOConnectCallStructMethod:return
/self->selector != 0/
{
    returns++;
    printf("RETURN time=%llu pid=%d tid=%d connection=%u selector=%u result=0x%x captured_output_size=%llu\n",
        timestamp, pid, tid, (uint32_t)self->connection, (uint32_t)self->selector, (uint32_t)arg1, (uint64_t)self->actual_size);
    self->selector = 0; self->connection = 0; self->output = 0;
    self->output_size = 0; self->capacity = 0; self->actual_size = 0;
}

dtrace:::ERROR
{
    faults++;
    printf("TRACE_FAULT probe=%d action=%d offset=%d fault=%d address=%p\n",
        (int)arg1, (int)arg2, (int)arg3, (int)arg4, (uintptr_t)arg5);
}

tick-10s
{
    exit(0);
}

dtrace:::END
{
    printf("TRACE_END enables=%d sends=%d gets=%d returns=%d faults=%d\n",
        enables, sends, receives, returns, faults);
}
