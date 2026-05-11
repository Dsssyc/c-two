from __future__ import annotations

import c_two as cc
from c_two.crm.contract import crm_contract, crm_contract_methods


def _contract_with_run_annotation(annotation: object):
    def run(self, value: annotation) -> annotation:  # type: ignore[valid-type]
        ...

    run.__annotations__ = {'value': annotation, 'return': annotation}
    return cc.crm(namespace='test.contract', version='0.1.0')(
        type('SharedContract', (), {'__module__': __name__, 'run': run}),
    )


def test_contract_signature_hash_distinguishes_method_annotations():
    int_contract = crm_contract(_contract_with_run_annotation(int))
    str_contract = crm_contract(_contract_with_run_annotation(str))

    assert int_contract.crm_ns == str_contract.crm_ns
    assert int_contract.crm_name == str_contract.crm_name
    assert int_contract.crm_ver == str_contract.crm_ver
    assert int_contract.abi_hash == str_contract.abi_hash
    assert int_contract.signature_hash != str_contract.signature_hash


def test_contract_abi_hash_excludes_shutdown_callback():
    @cc.crm(namespace='test.contract', version='0.1.0')
    class WithoutShutdown:
        def run(self, value: int) -> int:
            ...

    @cc.crm(namespace='test.contract', version='0.1.0')
    class WithShutdown:
        def run(self, value: int) -> int:
            ...

        @cc.on_shutdown
        def stop(self) -> None:
            ...

    assert crm_contract_methods(WithoutShutdown) == ['run']
    assert crm_contract_methods(WithShutdown) == ['run']
