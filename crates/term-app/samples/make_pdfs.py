"""Regenerate the small PDF viewer fixtures with ReportLab and Pillow."""
from pathlib import Path

from reportlab.pdfgen.canvas import Canvas

here = Path(__file__).parent
pdf = Canvas(str(here / "sample.pdf"), pagesize=(320, 128), invariant=True)
pdf.setTitle("Trident viewer sample")
pdf.setFillColorRGB(.08, .22, .26)
pdf.rect(0, 0, 320, 128, fill=1, stroke=0)
pdf.setFillColorRGB(.48, .89, .81)
pdf.setFont("Helvetica-Bold", 9)
pdf.drawString(16, 99, "TRIDENT    /    FIELD NOTES")
pdf.setFillColorRGB(1, 1, 1)
pdf.setFont("Helvetica", 24)
pdf.drawString(15, 59, "Tides of the Pacific")
pdf.setFont("Helvetica", 10)
pdf.drawString(16, 31, "01    Text  /  Vector shapes  /  Raster imagery")
pdf.showPage()
pdf.setFillColorRGB(.92, .97, .96)
pdf.rect(0, 0, 320, 128, fill=1, stroke=0)
pdf.setFillColorRGB(.08, .22, .26)
pdf.setFont("Helvetica-Bold", 11)
pdf.drawString(16, 101, "02    /    TIDAL OBSERVATIONS")
for i, height in enumerate((9, 19, 36, 55, 65, 51, 34, 23, 12)):
    pdf.setFillColorRGB(.05, .5, .48)
    pdf.rect(16 + 32 * i, 15, 23, height, fill=1, stroke=0)
pdf.showPage()
pdf.drawImage(str(here / "sample.png"), 0, 0, width=320, height=128)
pdf.showPage()
pdf.save()

locked = Canvas(str(here / "locked.pdf"), invariant=True, encrypt="viewer-test")
locked.drawString(16, 16, "Locked fixture")
locked.showPage()
locked.save()
