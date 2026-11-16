"""Closed file-only Scrapy fixture spider; run via scrapy runspider."""
from pathlib import Path
import scrapy

ROOT = Path(__file__).resolve().parents[2] / 'fixtures/s15'

class FixtureSpider(scrapy.Spider):
    name = 's15_fixture'
    custom_settings = {'TELNETCONSOLE_ENABLED': False, 'LOG_LEVEL': 'WARNING', 'CONCURRENT_REQUESTS': 1}

    async def start(self):
        for name in ('missing-title', 'has-title', 'missing-description', 'has-description'):
            yield scrapy.Request((ROOT / (name + '.html')).as_uri(), cb_kwargs={'name': name})

    def parse(self, response, name):
        yield {'profile': 's15-scrapy-fixture-v1', 'url': 'https://fixture.invalid/' + name,
               'file_status': response.status, 'title': response.css('title::text').get(),
               'description': response.css('meta[name="description"]::attr(content)').get()}
