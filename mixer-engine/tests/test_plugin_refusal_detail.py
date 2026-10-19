"""A refused plug-in insert replies with the stable `error` code plus a human `detail`."""
import json

from protocol import Command
from test_console_wiring import Writer
from test_plugin_native import SLOT, model
from ua_mixer_daemon import MixerClient, MixerDaemon


def test_refused_insert_reply_carries_the_detail():
    state, helper, driver, provider, backend, transactions = model()
    daemon = MixerDaemon(state, None, helper_tree=helper,
                         plugin_transactions=transactions, plugin_hosting=True)
    writer = Writer()
    client = MixerClient(None, writer, ubjson=False)
    daemon._handle_set(client, Command('set', SLOT + '/EffectName/value', 'No Such Plugin',
                                       raw_params={'func_id': '4'}))
    reply = json.loads(writer.frames[-1].rstrip(b'\0'))
    assert reply['error'] == 'openapollo.plugin_transaction_failed'
    assert reply['detail']
    assert reply['parameters']['func_id'] == 4
