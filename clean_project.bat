@echo off
chcp 65001 >nul

echo ===================================
echo 清理 flowdown 项目...
echo ===================================
cd /d "c:\Projects\flowdown"
call cargo clean
echo.
echo flowdown 清理完成！
echo.


