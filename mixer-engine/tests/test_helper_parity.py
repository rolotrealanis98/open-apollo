"""Engine-side details from the Mixer Helper / Engine comparison (N32,
N34)."""


def daemon(tree, router):
    from ua_mixer_daemon import MixerDaemon, apply_runtime_properties
    apply_runtime_properties(tree)
    return MixerDaemon(tree, router)


def test_dante_statuses_are_ints(tree, router):
    daemon(tree, router)
    for path in ("/DanteDevicesStatus", "/DanteErrorStatus"):
        assert tree.get_value(f"{path}/value") == 0
        assert tree.get_prop_dict(f"{path}/value")["type"] == "int"
