#!/bin/sh
# Kernel tuning for maximum network throughput.
# Non-fatal — works when privileged, silently skipped otherwise.
sysctl -w net.core.somaxconn=65535 2>/dev/null || true
sysctl -w net.ipv4.tcp_max_syn_backlog=65535 2>/dev/null || true
sysctl -w net.ipv4.tcp_fastopen=3 2>/dev/null || true
sysctl -w net.ipv4.tcp_tw_reuse=1 2>/dev/null || true
sysctl -w net.ipv4.tcp_fin_timeout=15 2>/dev/null || true
sysctl -w net.ipv4.tcp_slow_start_after_idle=0 2>/dev/null || true
sysctl -w net.core.netdev_max_backlog=65535 2>/dev/null || true
sysctl -w net.ipv4.ip_local_port_range="1024 65535" 2>/dev/null || true
sysctl -w net.core.rmem_max=16777216 2>/dev/null || true
sysctl -w net.core.wmem_max=16777216 2>/dev/null || true
sysctl -w net.ipv4.tcp_rmem="4096 87380 16777216" 2>/dev/null || true
sysctl -w net.ipv4.tcp_wmem="4096 65536 16777216" 2>/dev/null || true
sysctl -w net.core.busy_read=50 2>/dev/null || true
sysctl -w net.core.busy_poll=50 2>/dev/null || true
exec vortex
