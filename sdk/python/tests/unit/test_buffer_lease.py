from c_two import _native


def test_runtime_session_hold_counters_are_native_and_available_without_server():
    session = _native.RuntimeSession()
    stats = session.memory_stats()["holds"]
    assert stats["active_holds"] == 0
    assert stats["total_held_bytes"] == 0
    assert stats["oldest_hold_seconds"] == 0
    assert set(stats["by_storage"]) == {"inline", "shm", "handle", "file_spill"}
    assert set(stats["by_direction"]) == {"client_response", "resource_input"}


def test_runtime_session_lease_tracker_is_shared_by_stats():
    session = _native.RuntimeSession()
    tracker = session.lease_tracker()
    lease = tracker.track_retained(
        route_name="grid",
        method_name="subdivide_grids",
        direction="client_response",
        storage="inline",
        bytes=64,
    )
    assert session.memory_stats()["holds"]["active_holds"] == 1
    lease.release()
    assert session.memory_stats()["holds"]["active_holds"] == 0


def test_public_hold_counters_use_native_session_without_server():
    import c_two as cc
    import c_two.observe
    from c_two.transport import registry as registry_module

    cc.shutdown()
    registry_module._default_runtime = None  # noqa: SLF001
    stats = cc.observe.memory_stats()["holds"]
    assert stats["active_holds"] == 0
    assert stats["total_held_bytes"] == 0
    assert set(stats["by_storage"]) == {"inline", "shm", "handle", "file_spill"}
