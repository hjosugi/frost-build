@echo off
rem Windows half of the extension-neutral tools/gen_config launcher.
rem cmd.exe resolves this file through PATHEXT when frost.toml invokes
rem tools/gen_config; POSIX shells execute the sibling without an extension.
> "%~1" echo #ifndef FROST_SAMPLE_CONFIG_H
>> "%~1" echo #define FROST_SAMPLE_CONFIG_H
>> "%~1" echo #define FROST_GREETING "frost:"
>> "%~1" echo #endif
