"""Input compensation policy for synchronous native chains.

Descriptor latencies are samples, counted once per chain (including stereo).
The driver broadcasts the largest total latency in each synchronous group.
Record compensation uses latency through the recording tap; monitor compensation
uses that group's maximum. These are separate delay lines.
"""
from dataclasses import dataclass
from model_features import MAX_DELAY_COMP_SAMPLES

@dataclass(frozen=True)
class InputLatency:
    channels: tuple
    total: int
    record: int
    group: int = 0
    mode: int = 0
    span: int = 0

@dataclass(frozen=True)
class InputCompensation:
    channels: tuple
    record: int
    monitor: int
    capacity: int
    overloaded: bool
    required: int
    suggested: str | None


def plan_compensation(setting, inputs, *, quantum=32):
    """Compute ordinary 48k input policy from authoritative descriptor values.

    Group -1 opts out of synchronous monitor alignment. Off explicitly clears
    the compensation delays and suppresses the overload alert. The mode delay
    is already converted to samples by the descriptor owner.
    """
    if setting not in MAX_DELAY_COMP_SAMPLES or type(quantum) is not int or quantum not in (32,64):
        raise ValueError('supported compensation setting and quantum required')
    inputs=tuple(inputs);occupied=set();groups={}
    for item in inputs:
        if type(item) is not InputLatency or item.channels not in ((0,),(1,),(2,),(3,),(0,1),(2,3)):
            raise ValueError('explicit analog channel layout required')
        if any(type(c) is not int for c in item.channels) or occupied.intersection(item.channels):
            raise ValueError('disjoint input chains required')
        occupied.update(item.channels)
        if any(type(v) is not int or not 0<=v<=0x7fffffff for v in (item.total,item.record,item.mode,item.span)) or item.record>item.total:
            raise ValueError('nonnegative bounded descriptor latencies required')
        if type(item.group) is not int or not -1<=item.group<32:
            raise ValueError('synchronous group must be -1 or 0..31')
        if item.group>=0:groups[item.group]=max(groups.get(item.group,0),item.total)
    budget=MAX_DELAY_COMP_SAMPLES[setting];result=[]
    for item in inputs:
        required=max(item.total,item.span);overloaded=budget>0 and required>budget
        suggested=next((name for name,value in MAX_DELAY_COMP_SAMPLES.items() if value>0 and value>=required),None) if overloaded else None
        record=max(budget-item.record,0)+item.mode if budget else item.mode
        monitor=max(min(groups.get(item.group,0),budget)-item.total,0)+item.mode if budget else item.mode
        capacity=128 if budget else 0
        if budget:
            while capacity<budget+16+item.mode+quantum:capacity*=2
        if max(record,monitor)>=2048 or capacity>2048:
            raise ValueError('delay exceeds supported native carrier capacity')
        result.append(InputCompensation(item.channels,record,monitor,capacity,overloaded,required,suggested))
    return tuple(result)
