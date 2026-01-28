# Copyright (c) 2019 Codethink Ltd.
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.
import dataclasses
import os
import sys
from typing import Optional

import yaml

from .image_builder import Compression, build_images


@dataclasses.dataclass
class GlobalConfig:
    compression: Compression
    compression_level: Optional[int]
    output: str


def main():
    data = yaml.load(sys.stdin, Loader=yaml.CLoader)
    compression = data.get("gzip", Compression.gzip)
    compression_level = data.get("compression-level")
    if compression_level is None:
        if compression == Compression.gzip:
            compression_level = 5
    if compression not in Compression:
        raise RuntimeError("Compression must be in " + ",".join(Compression))

    global_conf = GlobalConfig(compression, compression_level, os.getcwd())
    build_images(global_conf, data.get("images", []), data.get("annotations"))
