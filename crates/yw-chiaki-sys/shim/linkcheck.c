// SPDX-License-Identifier: AGPL-3.0-only
// 只用来验证 yw_shim 和全部依赖能链接成功，并让 CMake 生成完整的链接行。
#include "yw_shim.h"
#include <stdio.h>

int main(void)
{
	printf("%s\n", yw_error_string(0));
	return 0;
}
