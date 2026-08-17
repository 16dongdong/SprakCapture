"""统一生成式工具栏位图的透明边距、外接尺寸和中心。"""

from pathlib import Path

from PIL import Image


canvasSize = 256
contentLongestEdge = 190


def normalizeIcon(iconPath: Path) -> None:
    """按统一最长边缩放并居中单个图标；空图像直接失败并保留源文件。"""
    sourceImage = Image.open(iconPath).convert("RGBA")
    visibleBounds = sourceImage.getchannel("A").getbbox()
    if visibleBounds is None:
        raise ValueError(f"图标没有可见像素：{iconPath}")

    glyphImage = sourceImage.crop(visibleBounds)
    # 工具栏只有 18px 可视空间，外接尺寸不一致会比线条面积差异更明显；统一最长边才能消除忽大忽小。
    scale = contentLongestEdge / max(glyphImage.width, glyphImage.height)
    glyphSize = (
        max(1, round(glyphImage.width * scale)),
        max(1, round(glyphImage.height * scale)),
    )
    glyphImage = glyphImage.resize(glyphSize, Image.Resampling.LANCZOS)
    normalizedImage = Image.new("RGBA", (canvasSize, canvasSize), (0, 0, 0, 0))
    glyphOffset = (
        (canvasSize - glyphSize[0]) // 2,
        (canvasSize - glyphSize[1]) // 2,
    )
    normalizedImage.alpha_composite(glyphImage, glyphOffset)
    normalizedImage.save(iconPath, optimize=True)


def main() -> None:
    """归一化 Web 公共目录内的全部工具栏状态图，确保同尺寸控件不会发生视觉漂移。"""
    serverRoot = Path(__file__).resolve().parents[1]
    iconDirectory = (
        serverRoot / "Frontend" / "Web" / "public" / "assets" / "toolbar"
    )
    iconPaths = sorted(iconDirectory.glob("*.png"))
    if not iconPaths:
        raise FileNotFoundError(f"工具栏图标目录为空：{iconDirectory}")
    for iconPath in iconPaths:
        normalizeIcon(iconPath)
        print(f"已对齐：{iconPath.name}")


if __name__ == "__main__":
    main()
