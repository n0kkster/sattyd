# sattyd
This fork has been made because I was really tired of waiting 650 ms every time I wanna take a screenshot. 
So a decision were taken to rewrite Satty like a daemon: preload GUI and other things once at start and 
then just display hidden window at every screenshot. Perfomance boost on my machine is about 10x (650 ms -> 60-70 ms)
and all time that it takes now is image loading and decoding.

Also copying edited image to clipboard also sped up a bit, using rust crate image and dedicating this work to
another thread. All examples and manuals in original repo.
