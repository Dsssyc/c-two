"""Server streams: one call, several items.

Demonstrates:
  - @cc.stream on a CRM method, implemented by a generator in the resource
  - iterating a cc.Stream while the resource produces the items
  - stopping early, which cancels the rest of the stream
  - close(), which reports how the stream ended

The resource and the client share one process here, so the items are passed
as Python objects. The same code works across processes: register the
resource in one process and pass its cc.server_address() to
cc.connect(..., address=...) in another, as ipc_resource.py and ipc_client.py
do for ordinary calls.

Run:
    uv run python examples/python/stream.py
"""

from collections.abc import Iterator

import c_two as cc


@cc.crm(namespace='demo.heat', version='0.1.0')
class HeatModel:
    @cc.stream
    def run(self, steps: int) -> Iterator[float]: ...

    @cc.read
    def temperature(self) -> float: ...


class Rod:
    """A one-value heat model whose temperature persists between calls."""

    def __init__(self) -> None:
        self._temperature = 20.0

    def run(self, steps: int) -> Iterator[float]:
        for _ in range(steps):
            self._temperature += 0.5
            yield self._temperature

    def temperature(self) -> float:
        return self._temperature


def main() -> None:
    cc.register(HeatModel, Rod(), name='rod')
    try:
        with cc.connect(HeatModel, name='rod') as model:
            # Each item arrives as soon as the generator yields it.
            stream = model.run(3)
            for value in stream:
                print(f'step: {value:.1f}')
            # A stream that ran to its end reports no error.
            print('ended:', stream.close())

            # The resource keeps its state between calls.
            print(f'temperature: {model.temperature():.1f}')

            # The next stream continues from that state. Leaving the loop
            # early and closing the stream cancels the steps that have not
            # started; the error names the consumer's cancellation. The
            # generator may already have produced one more item, which the
            # client never reads.
            stream = model.run(1000)
            for value in stream:
                print(f'step: {value:.1f}')
                if value >= 23.0:
                    break
            print('stopped:', stream.close())
    finally:
        cc.shutdown()


if __name__ == '__main__':
    main()
