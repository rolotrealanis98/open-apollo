/* Passive capture from an explicitly selected, authorized process.
 * Invoke: sudo dtrace -q -p PID -s cli_calls.d
 * SIP/hardened process restrictions can prevent attachment. A failed attachment
 * is not evidence that no CLI commands occurred. No device calls are issued.
 * ABI pinned to UA Plugin Support 12.0 arm64's direct StructMethod forwarding.
 */
#pragma D option strsize=320

pid$target::IOConnectCallStructMethod:entry
/arg1 == 173 && arg3 == 8/
{
    printf("ENABLE pid=%d index=%u enabled=%d\n", pid,
        *(uint32_t *)copyin(arg2, 4), *(int32_t *)copyin(arg2 + 4, 4));
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 174 && arg3 == 132/
{
    printf("SEND pid=%d index=%u command=%s\n", pid,
        *(uint32_t *)copyin(arg2, 4), copyinstr(arg2 + 4, 128));
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 175 && arg3 == 304/
{
    self->cli_output = arg4;
    self->cli_read = 1;
}

pid$target::IOConnectCallStructMethod:return
/self->cli_read && arg1 == 0/
{
    printf("RECEIVE pid=%d result=0 response=%s\n", pid,
        copyinstr(self->cli_output + 4, 300));
    self->cli_output = 0;
    self->cli_read = 0;
}

pid$target::IOConnectCallStructMethod:return
/self->cli_read && arg1 != 0/
{
    printf("RECEIVE pid=%d result=0x%x\n", pid, arg1);
    self->cli_output = 0;
    self->cli_read = 0;
}

tick-10s
{
    exit(0);
}
