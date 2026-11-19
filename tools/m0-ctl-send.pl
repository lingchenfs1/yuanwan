#!/usr/bin/perl
# 网关机上用：非阻塞地往会话的控制管道写一行命令。没有会话在读时直接报错，不会卡住。
use strict;
use Fcntl;
my $p = "$ENV{HOME}/yw-m0/ctl";
sysopen(my $f, $p, O_WRONLY | O_NONBLOCK) or die "没有会话在读控制管道（$!）\n";
my $line = join(" ", @ARGV) . "\n";
syswrite($f, $line) == length($line) or die "写入失败：$!\n";
print "已发送：", join(" ", @ARGV), "\n";
