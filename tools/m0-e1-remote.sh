#!/bin/bash
# E1：在网关机上运行（ssh ... 'bash -s' < tools/m0-e1-remote.sh），会话要先用 m0-remote.sh start 建好。
# 镜头来回转让画面持续运动；按时间表注入丢包上报，观察 PS5 的码率、FEC、分辨率反应。
set -u
send() { ~/yw-m0/ctl-send "$@" > /dev/null || exit 1; }
rm -f ~/yw-m0/e1.stop
(
	while [ ! -f ~/yw-m0/e1.stop ]; do
		~/yw-m0/ctl-send rstick 26000 0 900 > /dev/null 2>&1
		sleep 1
		~/yw-m0/ctl-send rstick -26000 0 900 > /dev/null 2>&1
		sleep 1
	done
) &
mover=$!
send mark E1 开始
send inject 0;    sleep 60
send inject 0.02; sleep 60
send inject 0.05; sleep 60
send inject 0.08; sleep 60
send inject 0.15; sleep 30
send inject 0;    sleep 90
send mark E1 结束
touch ~/yw-m0/e1.stop
wait "${mover}"
rm -f ~/yw-m0/e1.stop
echo "E1 完成"
