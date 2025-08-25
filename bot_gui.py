"""
Webook Bot - Browser Automation for Login and Seat Booking

Features:
- GUI for uploading users.csv and proxies.txt
- Browser automation using Playwright
- Token management with JSON persistence
- Seat booking functionality
- Multi-threaded operations

Dependencies:
    pip install playwright requests tkinter
    python -m playwright install chromium

CSV Format (users.csv):
    type,email,password
    premium,user@example.com,password123
"""

import tkinter as tk
from tkinter import ttk, filedialog, messagebox
import threading
import concurrent.futures
import queue
import csv
import json
import os
import requests
from pathlib import Path
from datetime import datetime, timedelta
import traceback
import secrets
import re
import hashlib
from urllib.parse import urlparse
import time
import websocket

# Playwright imports with error handling
try:
    from playwright.sync_api import sync_playwright, TimeoutError as PlaywrightTimeoutError
    PLAYWRIGHT_AVAILABLE = True
except ImportError:
    PLAYWRIGHT_AVAILABLE = False
    PlaywrightTimeoutError = Exception

# Configuration
CONFIG = {
    'tokens_file': 'tokens.json',
    'login_timeout': 60000,  # 30 seconds
    'navigation_timeout': 60000,  # 20 seconds
    'api_timeout': 60,  # 10 seconds for API calls
    'retry_delay': (1, 3),  # Random delay between retries
    'max_retries': 3,
}

class TokenManager:
    """Handles token storage and validation"""
    
    def __init__(self, tokens_file='tokens.json'):
        self.tokens_file = Path(tokens_file)
        self.tokens = self._load_tokens()
    
    def _load_tokens(self):
        """Load tokens from JSON file"""
        if not self.tokens_file.exists():
            return {}
        try:
            with open(self.tokens_file, 'r', encoding='utf-8') as f:
                return json.load(f)
        except (json.JSONDecodeError, FileNotFoundError):
            return {}
    
    def save_tokens(self):
        """Save tokens to JSON file"""
        try:
            with open(self.tokens_file, 'w', encoding='utf-8') as f:
                json.dump(self.tokens, f, indent=2, default=str)
            # Set restrictive permissions
            os.chmod(self.tokens_file, 0o600)
        except Exception as e:
            print(f"Failed to save tokens: {e}")
    
    def get_valid_token(self, email):
        """Get valid token for email, or None if expired/missing"""
        if email not in self.tokens:
            return None
        
        token_data = self.tokens[email]
        expires_at = token_data.get('expires_at')
        
        if expires_at:
            # Check if token expires in less than 1 hour (buffer time)
            if datetime.now() >= datetime.fromisoformat(expires_at) - timedelta(hours=1):
                return None
        
        return token_data
    
    def store_token(self, email, token_data):
        """Store token data for email"""
        # Calculate expiration time
        expires_in = token_data.get('token_expires_in', 604800)  # Default 7 days
        expires_at = datetime.now() + timedelta(seconds=expires_in)
        
        self.tokens[email] = {
            'access_token': token_data.get('access_token'),
            'refresh_token': token_data.get('refresh_token'),
            'token': token_data.get('token'),
            'expires_at': expires_at.isoformat(),
            'user_id': token_data.get('_id'),
            'created_at': datetime.now().isoformat()
        }
        self.save_tokens()

class WebookBot:
    """Main bot class for handling browser automation and API calls"""
    
    def __init__(self, email, password,headless=True, proxy=None):
        self.headless = headless
        self.proxy = proxy
        self.email = email
        self.password = password
        self.token_manager = TokenManager()
        self.session = requests.Session()
        self.browser_semaphore = threading.Semaphore(1)  # Only 1 browser at a time
        self.commitHash = None  # Placeholder for commit hash if needed
        self.chartToken = None
        self.chart_key = None
        self.event_key = None
        self.event_id = None
        self.channelKeyCommon = None
        self.channelKey = None
        self.home_team = None
        self.away_team = None
        self.browser_id = None
        self.channels = None
        self.seasonStructure = None
        self.webook_hold_token = None  # Placeholder for Webook hold token
        self.expireTime = 600
        
        # Setup session defaults
        #self.session.headers.update({
        #    'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
        #    'Accept': 'application/json, text/plain, */*',
        #    'Accept-Language': 'en-US,en;q=0.9',
        #    'Content-Type': 'application/json',
        #})
    

    def extract_seatsio_chart_data(self,log_callback=None):
        """
        Fetch SeatsIO chart.js and extract chartToken and commitHash
        Returns dict with chartToken and commitHash
        """
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        url = 'https://cdn-eu.seatsio.net/chart.js'
        
        headers = {
            'accept': '*/*',
            'accept-language': 'ar,en-US;q=0.9,en;q=0.8,fr;q=0.7',
            'sec-ch-ua': '"Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135"',
            'sec-ch-ua-mobile': '?0',
            'sec-ch-ua-platform': '"Linux"',
            'sec-fetch-dest': 'script',
            'sec-fetch-mode': 'no-cors',
            'sec-fetch-site': 'cross-site',
            'sec-fetch-storage-access': 'active',
            'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36'
        }
        
        try:
            log("Fetching SeatsIO chart.js...")
            session = requests.Session()
            if self.proxy:
                session.proxies = {
                    'http': self.proxy,
                    'https': self.proxy
                }
            
            response = session.get(url, headers=headers, timeout=30)
            response.raise_for_status()
            
            content = response.text
            log(f"Successfully fetched chart.js ({len(content)} bytes)")
            
            # Extract chartToken using regex
            chart_token_pattern = r"seatsio\.chartToken\s*=\s*['\"]([^'\"]+)['\"]"
            chart_token_match = re.search(chart_token_pattern, content)
            
            # Extract commitHash using regex  
            commit_hash_pattern = r"seatsio\.commitHash\s*=\s*['\"]([^'\"]+)['\"]"
            commit_hash_match = re.search(commit_hash_pattern, content)
            
            #result = {}
            
            if chart_token_match:
                self.chartToken = chart_token_match.group(1)
                log(f"✓ Found chartToken: {self.chartToken}")
            else:
                log("✗ chartToken not found")
                
            if commit_hash_match:
                self.commitHash = commit_hash_match.group(1)
                log(f"✓ Found commitHash: {self.commitHash}")
            else:
                log("✗ commitHash not found")
                
            # Also extract other useful data
            
            #return result
            
        except requests.exceptions.RequestException as e:
            error_msg = f"Failed to fetch chart.js: {str(e)}"
            log(error_msg)
            log(f"🔄 Retrying chart.js fetch...")
            time.sleep(2)  # Wait before retry
            
                # Try without proxy on last attempt
            return self.extract_seatsio_chart_data()
        except Exception as e:
            error_msg = f"Error extracting chart data: {str(e)}"
            log(error_msg)
            raise Exception(error_msg)
    def send_event_detail_request(self,event_id, lang='en', visible_in='rs', log_callback=None):
        """
        Send request to get event details from Webook API
        
        Args:
            event_id: Event ID (e.g., 'tpe-vs-nzl-fiba-asia-cup-2025-487336982')
            token: Authentication token
            lang: Language code (default: 'en')
            visible_in: Visibility parameter (default: 'rs')
            proxy: Proxy string (optional)
            log_callback: Logging function (optional)
        
        Returns:
            dict: API response
        """
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        # Build API URL
        api_url = f"https://api.webook.com/api/v2/event-detail/{event_id}"
        
        # Set up parameters
        params = {
            'lang': lang,
            'visible_in': visible_in
        }
        token = self.token_manager.get_valid_token(self.email)['token']
        # Set up headers (exactly like the curl command)
        headers = {
            'sec-ch-ua-platform': '"Linux"',
            'Referer': '',
            'sec-ch-ua': '"Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135"',
            'sec-ch-ua-mobile': '?0',
            'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36',
            'Accept': 'application/json',
            'Content-Type': 'application/json',
            'token': token
        }
        
        try:
            # Set up session with proxy if provided
            session = requests.Session()
            if self.proxy:
                session.proxies = {
                    'http': self.proxy,
                    'https': self.proxy
                }
            
            log(f"Sending request to: {api_url}")
            log(f"Token: {token[:20]}...")
            
            # Make the GET request
            response = session.get(
                api_url,
                params=params,
                headers=headers,
                timeout=30
            )
            
            log(f"Response status: {response.status_code}")
            
            # Return the response data
            try:
                response_data = response.json()
                log("✓ Response received successfully")
                #print(response_data)
                self.chart_key = response_data['data']['seats_io']['chart_key']
                self.event_key = response_data['data']['seats_io']['event_key']
                self.channelKeyCommon = response_data['data']['channel_keys']['common']
                self.channelKey = response_data['data']['channel_keys']
                self.event_id = response_data['data']['_id']
                self.home_team = response_data['data']['home_team']
                self.away_team = response_data['data']['away_team']
            except json.JSONDecodeError:
                return {
                    'status_code': response.status_code,
                    'data': response.text,
                    'success': False,
                    'error': 'Invalid JSON response'
                }
                
        except requests.exceptions.Timeout:
            error_msg = "Request timeout"
            log(f"✗ {error_msg}")
            return {
                'status_code': 0,
                'data': None,
                'success': False,
                'error': error_msg
            }
        except requests.exceptions.RequestException as e:
            error_msg = f"Network error: {str(e)}"
            log(f"✗ {error_msg}")
            return {
                'status_code': 0,
                'data': None,
                'success': False,
                'error': error_msg
            }
        except Exception as e:
            error_msg = f"Unexpected error: {str(e)}"
            log(f"✗ {error_msg}")
            return {
                'status_code': 0,
                'data': None,
                'success': False,
                'error': error_msg
            }
    def get_rendering_info(self, log_callback=None):
        """
        Fetch SeatsIO rendering info for a specific event
        
        Args:
            event_key: The event key (e.g., '9aug-tpe-vs-nzl-single-match')
            workspace_key: The workspace key (e.g., '3d443a0c-83b8-4a11-8c57-3db9d116ef76')
            log_callback: Optional callback for logging
        
        Returns:
            dict: The rendering info response
        """
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        
        # Generate browser ID if not exists
        if not self.browser_id:
            self.generate_browser_id()
        
        # Get commit hash (should be available from extract_seatsio_chart_data)
        
        url = f'https://cdn-eu.seatsio.net/system/public/3d443a0c-83b8-4a11-8c57-3db9d116ef76/rendering-info'
        
        headers = {
            'accept': '*/*',
            'accept-language': 'ar,en-US;q=0.9,en;q=0.8,fr;q=0.7',
            'priority': 'u=1, i',
            'referer': f'https://cdn-eu.seatsio.net/static/version/seatsio-ui-prod-00383-tzm/chart-renderer/chartRendererIframe.html?environment=PROD&commit_hash={self.commitHash}',
            'sec-ch-ua': '"Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135"',
            'sec-ch-ua-mobile': '?0',
            'sec-ch-ua-platform': '"Linux"',
            'sec-fetch-dest': 'empty',
            'sec-fetch-mode': 'cors',
            'sec-fetch-site': 'same-origin',
            'sec-fetch-storage-access': 'active',
            'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36',
            'x-browser-id': self.browser_id,
            'x-client-tool': 'Renderer',
            'x-request-origin': 'webook.com'
        }
        
        params = {
            'event_key': self.event_key
        }
        
        # Generate signature (this is a placeholder - you'll need the actual signing logic)
        signature = self.generate_signature()
        if signature:
            headers['x-signature'] = signature
        
        try:
            log(f"Using browser ID: {self.browser_id}")
            session = requests.Session()
            if self.proxy:
                session.proxies = {
                    'http': self.proxy,
                    'https': self.proxy
                }
            
            response = session.get(url, headers=headers, params=params, timeout=30)
            response.raise_for_status()
            
            data = response.json()
            log(f"✓ Successfully fetched rendering info")
            #print(data)
            all_objects = []
            seasonStructure = data.get('seasonStructure', {}).get('topLevelSeasonKey', None)
            print(f'seasonStructure: {seasonStructure}')
            for channel in data.get('channels', []):
                if 'objects' in channel:
                    all_objects.extend(channel['objects'])
            self.channels = all_objects
            return self.channels, seasonStructure
            
        except requests.exceptions.RequestException as e:
            error_msg = f"Failed to fetch rendering info: {str(e)}"
            log(error_msg)
            raise Exception(error_msg)
        except Exception as e:
            error_msg = f"Error getting rendering info: {str(e)}"
            log(error_msg)
            raise Exception(error_msg)
    def generate_browser_id(self):
        """
        Generate a browser ID similar to SeatsIO's format
        Returns a 16-character hexadecimal string
        """
        # Generate 8 random bytes and convert to hex
        random_bytes = secrets.token_bytes(8)
        browser_id = random_bytes.hex()
        
        # Store it for reuse in the session
        self.browser_id = browser_id
        return browser_id
    def generate_signature(self, request_body=""):
        """
        Generate x-signature header based on SeatsIO's oY function
        This function mimics: oY(this.chartToken, e.body || "")
        
        Args:
            request_body: The request body (empty string for GET requests)
        
        Returns:
            str: The signature hash
        """
        # Get chartToken (should be available from extract_seatsio_chart_data)
        if not self.chartToken:
            raise ValueError("chartToken is required for signature generation")
        
        # Convert request_body to string if it's not already
        if not isinstance(request_body, str):
            request_body = str(request_body) if request_body else ""
        
        # Reverse the chartToken
        reversed_token = self.chartToken[::-1]
        
        # Concatenate reversed token with request body
        data_to_hash = reversed_token + request_body
        
        # Generate SHA256 hash (not HMAC)
        signature = hashlib.sha256(data_to_hash.encode('utf-8')).hexdigest()
        
        return signature
    # In _assign_proxies method
    def _assign_proxies(self):
        if not self.proxies or not self.users:
            return
        
        # Shuffle proxies for better distribution
        import random
        random.shuffle(self.proxies)
        
        for i, user in enumerate(self.users):
            user['proxy'] = self.proxies[i % len(self.proxies)]
    def test_proxy(self, proxy_url):
        try:
            test_session = requests.Session()
            test_session.proxies = {'http': proxy_url, 'https': proxy_url}
            response = test_session.get('http://httpbin.org/ip', timeout=10)
            return response.status_code == 200
        except:
            return False
    def login_with_browser(self, event_url="", log_callback=None):
        """Login using browser automation and capture tokens"""
        if not PLAYWRIGHT_AVAILABLE:
            raise Exception("Playwright not installed. Run: pip install playwright && python -m playwright install chromium")
        
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        log(f"🔐 Starting browser session for {self.email}")
        
        # Check if we already have valid tokens
        existing_token = self.token_manager.get_valid_token(self.email)
        if existing_token:
            log(f"✓ Using existing valid token for {self.email}")
            return existing_token
        
        log("Opening playwright browser")
        with sync_playwright() as p:
            # Configure browser launch options
            browser_args = [
                '--no-sandbox',
                '--disable-setuid-sandbox', 
                '--disable-dev-shm-usage',
                '--disable-accelerated-2d-canvas',
                '--no-first-run',
                '--no-zygote',
                '--disable-gpu',
                '--disable-web-security',
                '--disable-features=VizDisplayCompositor',
                '--disable-blink-features=AutomationControlled'
            ]
            
            # Parse proxy configuration
            proxy_config = None
            if self.proxy:
                try:
                    #from urllib.parse import urlparse
                    parsed_proxy = urlparse(self.proxy)
                    
                    # Build proxy configuration for context
                    proxy_config = {
                        'server': f'{parsed_proxy.scheme}://{parsed_proxy.hostname}:{parsed_proxy.port}'
                    }
                    
                    # Add authentication if present
                    if parsed_proxy.username and parsed_proxy.password:
                        proxy_config['username'] = parsed_proxy.username
                        proxy_config['password'] = parsed_proxy.password
                    
                    log(f"🌐 Using proxy: {parsed_proxy.hostname}:{parsed_proxy.port}")
                    log(f"🔑 Proxy auth user: {parsed_proxy.username}")
                    
                except Exception as e:
                    log(f"Warning: Failed to parse proxy: {e}")
                    proxy_config = None
            
            # Launch browser WITHOUT proxy args (proxy will be set at context level)
            browser = p.chromium.launch(
                headless=self.headless,
                args=browser_args
            )
            
            try:
                # Create context WITH proxy configuration
                context_options = {
                    'viewport': {'width': 1520, 'height': 580},
                    'user_agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36',
                    'ignore_https_errors': True,  # Important for some proxies
                    'bypass_csp': True  # Bypass Content Security Policy
                }
                
                # Add proxy to context if available
                if proxy_config:
                    context_options['proxy'] = proxy_config
                    log("✅ Proxy configured at context level")
                
                context = browser.new_context(**context_options)
                
                # Add extra headers
                context.set_extra_http_headers({
                    'Accept-Language': 'en-US,en;q=0.9',
                    'Accept-Encoding': 'gzip, deflate, br',
                    'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8'
                })
                
                # Create page
                page = context.new_page()
                
                # Remove automation indicators
                page.add_init_script("""
                    Object.defineProperty(navigator, 'webdriver', {
                        get: () => undefined
                    });
                    
                    Object.defineProperty(navigator, 'plugins', {
                        get: () => [1, 2, 3, 4, 5]
                    });
                """)
                
                # Set up response handlers
                captured_tokens = {}
                
                def handle_response(response):
                    try:
                        #if 'api.webook.com' in response.url:
                        #    log(f"📡 API Response: {response.url.split('?')[0]} - Status: {response.status}")
                        
                        if 'api/v2/login' in response.url and response.request.method == 'POST':
                            log(f"🔐 Login response - Status: {response.status}")
                            
                            if response.status == 200:
                                try:
                                    data = response.json()
                                    if data.get('status') == 'success' and 'data' in data:
                                        captured_tokens.update(data['data'])
                                        log("✅ Login successful - tokens captured")
                                except Exception as e:
                                    log(f"Failed to parse response: {e}")
                            
                            elif response.status == 204:
                                log("✅ Login successful (204 response)")
                                
                    except Exception as e:
                        log(f"Error handling response: {e}")
                
                def handle_request(request):
                    try:
                        if 'api/v2/login' in request.url:
                            #print(request)
                            try:
                                #header = json.loads(request['headers'])
                                #print(f'header',request.headers)
                                #print(f'header token', request.headers.get('token'))
                                #print(request['headers']['token'])
                                captured_tokens['token'] = request.headers.get('token')
                                #log(f"🔑 Login attempt for: {header.get('token', 'unknown')}")
                            except:
                                pass
                    except Exception as e:
                        log(f"Error handling request: {e}")
                
                page.on('response', handle_response)
                page.on('request', handle_request)
                
                # Navigation and login
                try:
                    login_url = "https://webook.com/en/login"
                    if event_url:
                        login_url = f"https://webook.com/en/login?redirect=%2Fen"
                    
                    log(f"📍 Navigating to {login_url}")
                    
                    # Navigate with longer timeout for proxy
                    page.goto(login_url, timeout=60000, wait_until='domcontentloaded')
                    log("✅ Page loaded successfully")
                    
                    # Wait for page to stabilize
                    page.wait_for_timeout(2000)
                    
                    # Handle cookie consent
                    try:
                        cookie_button = page.locator('button:has-text("Accept all")')
                        if cookie_button.is_visible(timeout=3000):
                            cookie_button.click()
                            log("🍪 Cookie consent accepted")
                            page.wait_for_timeout(1000)
                    except:
                        pass
                    
                    # Wait for login form
                    log("⏳ Waiting for login form")
                    page.wait_for_selector('input[data-testid="auth_login_email_input"]', timeout=15000)
                    log("✅ Login form found")
                    
                    # Fill credentials
                    log("📝 Filling login credentials")
                    
                    email_input = page.locator('input[data-testid="auth_login_email_input"]')
                    email_input.click()
                    page.wait_for_timeout(300)
                    email_input.type(self.email, delay=10)
                    
                    page.wait_for_timeout(500)
                    password_input = page.locator('input[data-testid="auth_login_password_input"]')
                    password_input.click()
                    page.wait_for_timeout(300,)
                    password_input.type(self.password, delay=10)
                    
                    page.wait_for_timeout(500)
                    login_button = page.locator('button[data-testid="auth_login_submit_button"]')
                    
                    log("🚀 Submitting login form")
                    
                    # Click login and wait for response
                    with page.expect_response(
                        lambda r: 'api/v2/login' in r.url and r.request.method == 'POST',
                        timeout=60000
                    ) as response_info:
                        login_button.click()
                    
                    response = response_info.value
                    
                    # Wait for navigation/redirect
                    page.wait_for_timeout(3000)
                    
                    current_url = page.url
                    log(f"📍 Current URL after login: {current_url}")
                    
                                            # Store tokens
                    self.token_manager.store_token(self.email, captured_tokens)
                    log(f"💾 Tokens stored for {self.email}")
                        
                    return self.token_manager.get_valid_token(self.email)
                        
                except PlaywrightTimeoutError as e:
                    log(f"⏱️ Navigation timeout - this often indicates proxy issues")
                    raise Exception(f"Login timeout (check proxy): {str(e)}")
                except Exception as e:
                    log(f"❌ Login error: {str(e)}")
                    raise
                    
            finally:
                browser.close()
                log("🔒 Browser closed")
    
    def _handle_cookie_consent(self, page, log):
        """Handle cookie consent popup with the specific structure"""
        try:
            # Wait for cookie consent to appear
            cookie_consent_selector = 'div[id="cookie_consent"]'
            
            # Check if cookie consent exists and is visible
            if page.locator(cookie_consent_selector).count() > 0:
                log("Cookie consent banner found")
                
                # Look for "Accept all" button with specific text content
                accept_button_selectors = [
                    'button:has-text("Accept all")',
                    'div[id="cookie_consent"] button:has-text("Accept all")',
                    'button:text("Accept all")'
                ]
                
                for selector in accept_button_selectors:
                    try:
                        accept_button = page.locator(selector)
                        if accept_button.count() > 0 and accept_button.is_visible():
                            log(f"Clicking Accept all button using selector: {selector}")
                            accept_button.click(timeout=5000)
                            log("✓ Cookie consent accepted")
                            
                            # Wait for the banner to disappear
                            page.wait_for_selector(cookie_consent_selector, state='hidden', timeout=5000)
                            page.wait_for_timeout(1000)
                            return
                    except Exception as e:
                        log(f"Failed with selector {selector}: {e}")
                        continue
                
                log("Could not find or click Accept all button")
            else:
                log("No cookie consent banner found")
                
        except Exception as e:
            log(f"Cookie consent handling error: {e}")
    def getWebookHoldToken(self, lang='en', log_callback=None):
        """
        Get hold token from Webook API for seat booking
        
        Args:
            event_id: Webook event ID (e.g., '684ffd1f165ecfe8710e6d71')
            lang: Language code (default: 'en')
            log_callback: Optional callback for logging
        
        Returns:
            dict: Response containing hold token and details
        """
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        # Get tokens from token manager
        token_data = self.token_manager.get_valid_token(self.email)
        if not token_data:
            raise ValueError(f"No valid token for {self.email}. Login required.")
        
        access_token = token_data.get('access_token')
        token = token_data.get('token')
        
        if not access_token or not token:
            raise ValueError("Missing required tokens (access_token or token)")
        
        # Build URL with query parameter
        url = f"https://api.webook.com/api/v2/seats/hold-token?lang={lang}"
        
        # Prepare request body
        request_body = {
            "event_id": self.event_id,
            "lang": lang
        }
        
        # Prepare headers
        headers = {
            'accept': 'application/json',
            'accept-language': 'ar,en-US;q=0.9,en;q=0.8,fr;q=0.7',
            'authorization': f'Bearer {access_token}',
            'content-type': 'application/json',
            'origin': 'https://webook.com',
            'priority': 'u=1, i',
            'sec-ch-ua': '"Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135"',
            'sec-ch-ua-mobile': '?0',
            'sec-ch-ua-platform': '"Linux"',
            'sec-fetch-dest': 'empty',
            'sec-fetch-mode': 'cors',
            'sec-fetch-site': 'same-site',
            'token': token,
            'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36'
        }
        
        try:
            log(f"Requesting Webook hold token for event: {self.event_id}")
            session = requests.Session()
            if self.proxy:
                session.proxies = {
                    'http': self.proxy,
                    'https': self.proxy
                }
            # Make the POST request
            response = session.post(
                url,
                headers=headers,
                json=request_body,
                timeout=30
            )
            
            log(f"Response status: {response.status_code}")
            
            if response.status_code == 200:
                response_data = response.json()
                log("✓ Webook hold token received successfully")
                #print(response_data)
                log(response_data.get('data').get('token'))
                
                # Store the Webook hold token
                self.webook_hold_token = response_data.get('data').get('token')
                log(f'Webook hold token: {self.webook_hold_token}')
                
            else:
                error_msg = f"Failed to get Webook hold token: HTTP {response.status_code}"
                try:
                    error_data = response.json()
                    error_msg += f" - {error_data.get('message', error_data)}"
                except:
                    error_msg += f" - {response.text[:200]}"
                
                log(f"✗ {error_msg}")
                
        except requests.exceptions.Timeout:
            error_msg = "Request timeout while getting Webook hold token"
            log(f"✗ {error_msg}")
        except requests.exceptions.RequestException as e:
            error_msg = f"Network error while getting Webook hold token: {str(e)}"
            log(f"✗ {error_msg}")
        except Exception as e:
            error_msg = f"Unexpected error while getting Webook hold token: {str(e)}"
            log(f"✗ {error_msg}")
    def getExpireTime(self, log_callback=None):
        """
        Get information about a SeatsIO hold token
        
        Args:
            hold_token: The SeatsIO hold token (e.g., 'e803c179-d9ee-4037-83fd-b6ad83eb1e28')
            log_callback: Optional callback for logging
        
        Returns:
            dict: Response containing hold token information
        """
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        # Validate prerequisites
        if not self.chartToken:
            raise ValueError("chartToken not available. Run extract_seatsio_chart_data() first")
        
        # Generate browser ID if not exists
        if not self.browser_id:
            self.generate_browser_id()
            log(f"Generated browser ID: {self.browser_id}")
        
        # Generate signature for empty body (GET request)
        signature = self.generate_signature("")
        
        # Build URL
        workspace_key = "3d443a0c-83b8-4a11-8c57-3db9d116ef76"
        url = f"https://cdn-eu.seatsio.net/system/public/{workspace_key}/hold-tokens/{self.webook_hold_token}"
        
        # Prepare headers
        headers = {
            'accept': '*/*',
            'accept-language': 'ar,en-US;q=0.9,en;q=0.8,fr;q=0.7',
            'priority': 'u=1, i',
            'referer': f'https://cdn-eu.seatsio.net/static/version/seatsio-ui-prod-00384-f7t/chart-renderer/chartRendererIframe.html?environment=PROD&commit_hash={self.commitHash}',
            'sec-ch-ua': '"Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135"',
            'sec-ch-ua-mobile': '?0',
            'sec-ch-ua-platform': '"Linux"',
            'sec-fetch-dest': 'empty',
            'sec-fetch-mode': 'cors',
            'sec-fetch-site': 'same-origin',
            'sec-fetch-storage-access': 'active',
            'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36',
            'x-browser-id': self.browser_id,
            'x-client-tool': 'Renderer',
            'x-signature': signature
        }
        
        try:
            log(f"Getting info for expire time of hold token: {self.webook_hold_token}")
            session = requests.Session()
            if self.proxy:
                session.proxies = {
                    'http': self.proxy,
                    'https': self.proxy
                }
            # Make the GET request
            response = session.get(
                url,
                headers=headers,
                timeout=30
            )
            
            log(f"Response status: {response.status_code}")
            
            if response.status_code == 200:
                try:
                    response_data = response.json()
                    log("✓ Expire time of hold token info retrieved successfully")
                    log(response_data.get('expiresInSeconds', 600))
                    self.expireTime = response_data.get('expiresInSeconds', 600)  # Default to 600 seconds if not found
                except json.JSONDecodeError:
                    log("✓ Expire time of hold token info retrieved (non-JSON response)")
            else:
                error_msg = f"Failed to get expire time hold token info: HTTP {response.status_code}"
                try:
                    error_data = response.json()
                    error_msg += f" - {error_data}"
                except:
                    error_msg += f" - {response.text[:200]}"
                
                log(f"✗ {error_msg}")
                
        except requests.exceptions.Timeout:
            error_msg = "Request timeout while getting Expire time of hold token info"
        except requests.exceptions.RequestException as e:
            error_msg = f"Network error while getting Expire time of hold token info: {str(e)}"
            log(f"✗ {error_msg}")
        except Exception as e:
            error_msg = f"Unexpected error while getting expire time of hold token info: {str(e)}"
            log(f"✗ {error_msg}")
    def takeSeat(self, seat_objects, event_keys=None, channel_keys=None, log_callback=None):
        """
        Hold/Reserve seats on SeatsIO
        
        Args:
            seat_objects: List of seat IDs to hold (e.g., ['j-9-14', 'j-9-15'])
            event_keys: List of event keys (defaults to self.event_key if not provided)
            channel_keys: List of channel keys (defaults to ['NO_CHANNEL'])
            log_callback: Optional callback for logging
        
        Returns:
            dict: Response containing hold status and details
        """
        
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        # Validate prerequisites
        if not self.chartToken:
            raise ValueError("chartToken not available. Run extract_seatsio_chart_data() first")
        if not self.event_key and not event_keys:
            raise ValueError("event_key not available. Run send_event_detail_request() first")
        
        # Generate browser ID if not exists
        if not self.browser_id:
            self.generate_browser_id()
            log(f"Generated browser ID: {self.browser_id}")
        
        # Use provided event_keys or default to self.event_key
        if not event_keys:
            event_keys = [self.event_key]
        
        # Use provided channel_keys or default
        if not channel_keys:
            channel_keys = ["NO_CHANNEL", *self.channelKeyCommon, *self.channelKey.get(self.home_team['_id'])]
        
        # Generate a unique hold token (UUID v4)
        hold_token = self.webook_hold_token
        log(f"hold token: {hold_token}")
        
        # Prepare the request body
        request_body = {
            "events": event_keys,
            "holdToken": hold_token,
            "objects": [{"objectId": seat_objects}],
            "channelKeys": channel_keys,
            "validateEventsLinkedToSameChart": True
        }
        
        # Convert to JSON string for signature
        request_body_str = json.dumps(request_body, separators=(',', ':'))
        
        # Generate signature for this specific request
        signature = self.generate_signature(request_body_str)
        log(f"Generated signature: {signature[:20]}...")
        
        # Build the URL (using workspace key from rendering info URL)
        workspace_key = "3d443a0c-83b8-4a11-8c57-3db9d116ef76"  # This should match your workspace
        url = f"https://cdn-eu.seatsio.net/system/public/{workspace_key}/events/groups/actions/hold-objects"
        
        # Prepare headers
        headers = {
            'accept': '*/*',
            'accept-language': 'ar,en-US;q=0.9,en;q=0.8,fr;q=0.7',
            'content-type': 'application/json',
            'origin': 'https://cdn-eu.seatsio.net',
            'priority': 'u=1, i',
            'referer': f'https://cdn-eu.seatsio.net/static/version/seatsio-ui-prod-00384-f7t/chart-renderer/chartRendererIframe.html?environment=PROD&commit_hash={self.commitHash}',
            'sec-ch-ua': '"Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135"',
            'sec-ch-ua-mobile': '?0',
            'sec-ch-ua-platform': '"Linux"',
            'sec-fetch-dest': 'empty',
            'sec-fetch-mode': 'cors',
            'sec-fetch-site': 'same-origin',
            'sec-fetch-storage-access': 'active',
            'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36',
            'x-browser-id': self.browser_id,
            'x-client-tool': 'Renderer',
            'x-signature': signature
        }
        
        try:
            log(f"Attempting to hold {seat_objects}")
            log(f"For event(s): {', '.join(event_keys)}")
            #print(f'url', url)
            #print(f'headers', headers)
            #print(f'request_body_str', request_body_str)
            session = requests.Session()
            if self.proxy:
                session.proxies = {
                    'http': self.proxy,
                    'https': self.proxy
                }
            # Make the POST request
            response = session.post(
                url,
                headers=headers,
                data=request_body_str,  # Send as string, not dict
                timeout=30
            )
            
            log(f"Response status: {response.status_code}")
            
            if response.status_code == 200 or response.status_code == 204:
                try:
                    response_data = response.json()
                    log("✓ Seats successfully held")
                    
                    # Store hold token for later use (booking/release)
                    self.current_hold_token = hold_token
                    
                    return {
                        'success': True,
                        'status_code': response.status_code,
                        'data': response_data,
                        'hold_token': hold_token,
                        'seats_held': seat_objects
                    }
                except json.JSONDecodeError:
                    log("✓ Seats held (non-JSON response)")
                    return {
                        'success': True,
                        'status_code': response.status_code,
                        'data': response.text,
                        'hold_token': hold_token,
                        'seats_held': seat_objects
                    }
            else:
                error_msg = f"Failed to hold seats: HTTP {response.status_code}"
                try:
                    error_data = response.json()
                    error_msg += f" - {error_data}"
                except:
                    error_msg += f" - {response.text[:200]}"
                
                log(f"✗ {error_msg}")
                return {
                    'success': False,
                    'status_code': response.status_code,
                    'error': error_msg,
                    'hold_token': hold_token
                }
                
        except requests.exceptions.Timeout:
            error_msg = "Request timeout while holding seats"
            log(f"✗ {error_msg}")
            return {
                'success': False,
                'status_code': 0,
                'error': error_msg,
                'hold_token': hold_token
            }
        except requests.exceptions.RequestException as e:
            error_msg = f"Network error while holding seats: {str(e)}"
            log(f"✗ {error_msg}")
            return {
                'success': False,
                'status_code': 0,
                'error': error_msg,
                'hold_token': hold_token
            }
        except Exception as e:
            error_msg = f"Unexpected error while holding seats: {str(e)}"
            log(f"✗ {error_msg}")
            return {
                'success': False,
                'status_code': 0,
                'error': error_msg,
                'hold_token': hold_token
            }
    def releaseSeat(self, seat_objects, hold_token, event_keys=None, log_callback=None):
        """
        Release previously held seats on SeatsIO
        
        Args:
            seat_objects: List of seat IDs to release (e.g., ['j-9-14', 'j-9-15'])
            hold_token: The hold token from takeSeat
            event_keys: List of event keys (defaults to self.event_key if not provided)
            log_callback: Optional callback for logging
        
        Returns:
            dict: Response containing release status and details
        """
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        # Validate prerequisites
        if not self.chartToken:
            raise ValueError("chartToken not available. Run extract_seatsio_chart_data() first")
        if not self.event_key and not event_keys:
            raise ValueError("event_key not available. Run send_event_detail_request() first")
        
        # Generate browser ID if not exists
        if not self.browser_id:
            self.generate_browser_id()
            log(f"Generated browser ID: {self.browser_id}")
        
        # Use provided event_keys or default to self.event_key
        if not event_keys:
            event_keys = [self.event_key]
        
        # Prepare the request body (same structure as hold)
        request_body = {
            "events": event_keys,
            "holdToken": hold_token,
            "objects": [{"objectId": seat_objects}],
            "validateEventsLinkedToSameChart": True
        }
        
        # Convert to JSON string for signature
        request_body_str = json.dumps(request_body, separators=(',', ':'))
        
        # Generate signature for this specific request
        signature = self.generate_signature(request_body_str)
        log(f"Generated signature for release: {signature[:20]}...")
        
        # Build the URL
        workspace_key = "3d443a0c-83b8-4a11-8c57-3db9d116ef76"
        url = f"https://cdn-eu.seatsio.net/system/public/{workspace_key}/events/groups/actions/release-held-objects"
        
        # Prepare headers (same as takeSeat)
        headers = {
            'accept': '*/*',
            'accept-language': 'ar,en-US;q=0.9,en;q=0.8,fr;q=0.7',
            'content-type': 'application/json',
            'origin': 'https://cdn-eu.seatsio.net',
            'priority': 'u=1, i',
            'referer': f'https://cdn-eu.seatsio.net/static/version/seatsio-ui-prod-00384-f7t/chart-renderer/chartRendererIframe.html?environment=PROD&commit_hash={self.commitHash}',
            'sec-ch-ua': '"Google Chrome";v="135", "Not-A.Brand";v="8", "Chromium";v="135"',
            'sec-ch-ua-mobile': '?0',
            'sec-ch-ua-platform': '"Linux"',
            'sec-fetch-dest': 'empty',
            'sec-fetch-mode': 'cors',
            'sec-fetch-site': 'same-origin',
            'sec-fetch-storage-access': 'active',
            'user-agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36',
            'x-browser-id': self.browser_id,
            'x-client-tool': 'Renderer',
            'x-signature': signature
        }
        
        try:
            log(f"Attempting to release {seat_objects}")
            log(f"Using hold token: {hold_token}")
            log(f"For event(s): {', '.join(event_keys)}")
            session = requests.Session()
            if self.proxy:
                session.proxies = {
                    'http': self.proxy,
                    'https': self.proxy
                }
            # Make the POST request
            response = session.post(
                url,
                headers=headers,
                data=request_body_str,  # Send as string, not dict
                timeout=30
            )
            
            log(f"Response status: {response.status_code}")
            
            if response.status_code == 200 or response.status_code == 204:
                try:
                    response_data = response.json()
                    log("✓ Seats successfully released")
                    
                    # Clear the stored hold token if it matches
                    if hasattr(self, 'current_hold_token') and self.current_hold_token == hold_token:
                        self.current_hold_token = None
                    
                    return {
                        'success': True,
                        'status_code': response.status_code,
                        'data': response_data,
                        'seats_released': seat_objects
                    }
                except json.JSONDecodeError:
                    log("✓ Seats released (non-JSON response)")
                    return {
                        'success': True,
                        'status_code': response.status_code,
                        'data': response.text,
                        'seats_released': seat_objects
                    }
            else:
                error_msg = f"Failed to release seats: HTTP {response.status_code}"
                try:
                    error_data = response.json()
                    error_msg += f" - {error_data}"
                except:
                    error_msg += f" - {response.text[:200]}"
                
                log(f"✗ {error_msg}")
                return {
                    'success': False,
                    'status_code': response.status_code,
                    'error': error_msg
                }
                
        except requests.exceptions.Timeout:
            error_msg = "Request timeout while releasing seats"
            log(f"✗ {error_msg}")
            return {
                'success': False,
                'status_code': 0,
                'error': error_msg
            }
        except requests.exceptions.RequestException as e:
            error_msg = f"Network error while releasing seats: {str(e)}"
            log(f"✗ {error_msg}")
            return {
                'success': False,
                'status_code': 0,
                'error': error_msg
            }
        except Exception as e:
            error_msg = f"Unexpected error while releasing seats: {str(e)}"
            log(f"✗ {error_msg}")
            return {
                'success': False,
                'status_code': 0,
                'error': error_msg
            }
    def _extract_event_id(self, event_url):
        """Extract event ID from URL - adjust based on actual URL format"""
        # For URL like: https://webook.com/en/events/semi-final-fiba-2-2025-fhiejsuhf98e/book
        try:
            parts = event_url.rstrip('/').split('/')
            if 'events' in parts:
                event_index = parts.index('events')
                if event_index + 1 < len(parts):
                    return parts[event_index + 1]
        except:
            pass
        return "unknown-event-id"


class SeatScanner:
    """WebSocket-based seat scanning system for real-time seat monitoring"""
    
    def __init__(self, workspace_key="3d443a0c-83b8-4a11-8c57-3db9d116ef76", event_key=None,seasonStructure = None):
        self.workspace_key = workspace_key
        self.event_key = event_key
        self.ws = None
        self.is_connected = False
        self.is_running = False
        
        # Seat tracking
        self.reserved_seats_map = {}  # holdTokenHash -> list of seats
        self.users_by_type = {}  # type -> list of users
        self.log_callback = None
        
        # Channels
        self.workspace_channel = workspace_key
        self.event_channel = f"{workspace_key}-{event_key}" if event_key else None
        self.event_channel2 = f"{workspace_key}-{seasonStructure}" if seasonStructure else None
        
    def set_event_key(self, event_key, seasonStructure):
        """Set the event key and update event channel"""
        self.event_key = event_key
        self.event_channel = f"{self.workspace_key}-{event_key}"
        self.event_channel2 = f"{self.workspace_key}-{seasonStructure}"
        self.log(f"Event channel updated: {self.event_channel}")
    
    def set_users(self, users):
        """Set users and group them by type"""
        self.users_by_type = {}
        for user in users:
            user_type = user.get('type', '*')
            if user_type not in self.users_by_type:
                self.users_by_type[user_type] = []
            self.users_by_type[user_type].append(user)
        
        # Log how many users of each type are available for scanning
        for user_type, type_users in self.users_by_type.items():
            self.log(f"📊 Type '{user_type}': {len(type_users)} users ready for scanning")
    
    def set_log_callback(self, callback):
        """Set logging callback function"""
        self.log_callback = callback
    
    def log(self, message):
        """Log message with timestamp"""
        timestamp = datetime.now().strftime('%H:%M:%S')
        log_msg = f"[{timestamp}] [SCANNER] {message}"
        if self.log_callback:
            self.log_callback(log_msg)
        print(log_msg)
    
    def start_scanning(self):
        """Start the WebSocket scanning system"""
        if self.is_running:
            self.log("Scanner already running")
            return
            
        if not self.event_key:
            self.log("❌ Cannot start scanner: event_key not set")
            return
        
        self.is_running = True
        self.log("Starting seat scanner...")
        
        # Start WebSocket connection in a separate thread
        scanner_thread = threading.Thread(target=self._run_websocket, daemon=True)
        scanner_thread.start()
    
    def stop_scanning(self):
        """Stop the scanning system"""
        self.is_running = False
        if self.ws:
            self.ws.close()
        self.log("Scanner stopped")
    
    def _run_websocket(self):
        """Run WebSocket connection with auto-reconnect"""
        while self.is_running:
            try:
                self._connect_websocket()
                time.sleep(5)  # Wait before reconnecting
            except Exception as e:
                self.log(f"WebSocket error: {str(e)}")
                time.sleep(5)
    
    def _connect_websocket(self):
        """Establish WebSocket connection"""
        ws_url = "wss://messaging-eu.seatsio.net/ws"
        
        headers = {
            'Origin': 'https://cdn-eu.seatsio.net',
            'Cache-Control': 'no-cache',
            'Accept-Language': 'ar,en-US;q=0.9,en;q=0.8,fr;q=0.7',
            'Pragma': 'no-cache',
            'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36',
            #'Sec-WebSocket-Extensions': 'permessage-deflate; client_max_window_bits'
        }
        
        self.ws = websocket.WebSocketApp(
            ws_url,
            header=headers,
            on_open=self._on_open,
            on_message=self._on_message,
            on_error=self._on_error,
            on_close=self._on_close
        )
        self.ws.compression = None
        self.ws.run_forever()
    
    def _on_open(self, ws):
        """Handle WebSocket connection opened"""
        self.is_connected = True
        self.log("✓ WebSocket connected")
        
        # Subscribe to channels
        self._subscribe_to_channels()
    
    def _subscribe_to_channels(self):
        """Subscribe to workspace and event channels"""
        # Subscribe to workspace channel
        workspace_sub = {
            "type": "SUBSCRIBE",
            "data": {"channel": self.workspace_channel}
        }
        self.ws.send(json.dumps(workspace_sub))
        self.log(f"📡 Subscribed to workspace channel: {self.workspace_channel}")
        
        # Subscribe to event channel (if event_key is set)
        if self.event_channel:
            event_sub = {
                "type": "SUBSCRIBE", 
                "data": {"channel": self.event_channel}
            }
            self.ws.send(json.dumps(event_sub))
            self.log(f"📡 Subscribed to event channel: {self.event_channel}")
        elif self.event_channel2:
            event_sub = {
                "type": "SUBSCRIBE", 
                "data": {"channel": self.event_channel2}
            }
            self.ws.send(json.dumps(event_sub))
            self.log(f"📡 Subscribed to event channel: {self.event_channel2}")
        else:
            self.log("⚠️ Event channel not available - event_key not set")
    
    def _on_message(self, ws, message):
        """Handle incoming WebSocket messages"""
        try:
            data = json.loads(message)
            print(data)
            
            # Handle array of messages
            if isinstance(data, list):
                for msg in data:
                    self._process_message(msg)
            else:
                self._process_message(data)
                
        except json.JSONDecodeError as e:
            self.log(f"Failed to parse message: {str(e)}")
        except Exception as e:
            self.log(f"Error processing message: {str(e)}")
    
    def _process_message(self, msg):
        """Process individual message"""
        if msg.get('type') != 'MESSAGE_PUBLISHED':
            return
        
        message = msg.get('message', {})
        channel = message.get('channel')
        body = message.get('body', {})
        
        # Handle workspace channel messages (hold token expiration)
        if channel == self.workspace_channel:
            self._handle_workspace_message(body)
        
        # Handle event channel messages (seat status changes)
        elif channel == self.event_channel:
            self._handle_event_message(body)
    
    def _handle_workspace_message(self, body):
        """Handle workspace channel messages (hold token expiration)"""
        message_type = body.get('messageType')
        
        if message_type == 'HOLD_TOKEN_EXPIRED':
            hold_token = body.get('holdToken')
            if hold_token:
                self._handle_hold_token_expired(hold_token)
    
    def _handle_event_message(self, body):
        """Handle event channel messages (seat status changes)"""
        status = body.get('status')
        seat_id = body.get('objectLabelOrUuid')
        hold_token_hash = body.get('holdTokenHash')
        
        if not seat_id:
            return
        #if seat_id
        if status == 'free':
            self._handle_seat_free(seat_id)
        elif status == 'reservedByToken' and hold_token_hash:
            self._handle_seat_reserved(seat_id, hold_token_hash)
    
    def _handle_seat_free(self, seat_id):
        """Handle when a seat becomes free - trigger * users to attempt booking"""
        self.log(f"🟢 SEAT FREE: {seat_id}")
        
        # Get users with type '*'
        star_users = self.users_by_type.get('*', [])
        
        if star_users:
            # Try to book with all * users simultaneously
            self._attempt_seat_booking(star_users, seat_id, "free_seat")
    
    def _handle_seat_reserved(self, seat_id, hold_token_hash):
        """Handle when a seat is reserved - track the reservation"""
        self.log(f"🟡 SEAT RESERVED: {seat_id} (token: {hold_token_hash[:12]}...)")
        
        # Add to reserved seats map
        if hold_token_hash not in self.reserved_seats_map:
            self.reserved_seats_map[hold_token_hash] = []
        
        if seat_id not in self.reserved_seats_map[hold_token_hash]:
            self.reserved_seats_map[hold_token_hash].append(seat_id)
            self.log(f"📝 Tracked seat {seat_id} for token {hold_token_hash[:12]}...")
    
    def _handle_hold_token_expired(self, hold_token):
        """Handle hold token expiration - trigger ** users for tracked seats"""
        # Generate SHA1 hash of the hold token
        hold_token_hash = hashlib.sha1(hold_token.encode()).hexdigest()
        
        self.log(f"⏰ HOLD TOKEN EXPIRED: {hold_token[:12]}... (hash: {hold_token_hash[:12]}...)")
        
        # Check if we have tracked seats for this token
        if hold_token_hash in self.reserved_seats_map:
            expired_seats = self.reserved_seats_map[hold_token_hash]
            self.log(f"🎯 Found {len(expired_seats)} seats to attempt: {expired_seats}")
            
            # Get users with type '**'
            # Get users with type '*'
            star_users = self.users_by_type.get('*', [])

            if star_users and expired_seats:
                # Try to book expired seats with * users
                for seat_id in expired_seats:
                    self._attempt_seat_booking(star_users, seat_id, "expired_token")
            
            # Remove from tracking map
            del self.reserved_seats_map[hold_token_hash]
            self.log(f"🗑️ Removed tracking for expired token {hold_token_hash[:12]}...")
        else:
            self.log(f"ℹ️ No tracked seats found for expired token {hold_token_hash[:12]}...")
    
    def _attempt_seat_booking(self, users, seat_id, trigger_type):
        """Attempt to book a seat with multiple users"""
        self.log(f"🚀 BOOKING ATTEMPT: {seat_id} with {len(users)} users (trigger: {trigger_type})")
        
        # Use ThreadPoolExecutor for concurrent booking attempts
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(users)) as executor:
            futures = []
            
            for user in users:
                bot = user.get('bot_instance')
                if bot and hasattr(bot, 'takeSeat'):
                    future = executor.submit(self._book_seat_for_user, user, bot, seat_id, trigger_type)
                    futures.append(future)
            
            # Process results
            for future in concurrent.futures.as_completed(futures, timeout=10):
                try:
                    future.result()
                except Exception as e:
                    self.log(f"Booking error: {str(e)}")
    
    def _book_seat_for_user(self, user, bot, seat_id, trigger_type):
        """Book seat for a specific user"""
        email = user.get('email', 'unknown')
        
        def user_log(msg):
            timestamp = datetime.now().strftime('%H:%M:%S')
            log_entry = f"[{timestamp}] [SCANNER-{trigger_type.upper()}] {msg}"
            if 'logs' in user:
                user['logs'].append(log_entry)
            self.log(f"[{email}] {msg}")
        
        try:
            user_log(f"→ Attempting to book {seat_id}")
            
            result = bot.takeSeat(seat_id, log_callback=user_log)
            
            if result and result.get('success'):
                user_log(f"✅ SUCCESS: Booked {seat_id}")
                
                # Update user's seat count
                user['seats_booked'] = user.get('seats_booked', 0) + 1
                if 'assigned_seats' not in user:
                    user['assigned_seats'] = []
                user['assigned_seats'].append(seat_id)
                
                return True
            else:
                user_log(f"❌ FAILED: Could not book {seat_id}")
                return False
                
        except Exception as e:
            user_log(f"💥 ERROR: {str(e)}")
            return False
    
    def _on_error(self, ws, error):
        """Handle WebSocket errors"""
        self.log(f"❌ WebSocket error: {str(error)}")
        self.is_connected = False
    
    def _on_close(self, ws, close_status_code, close_msg):
        """Handle WebSocket connection closed"""
        self.log(f"🔌 WebSocket closed: {close_status_code} - {close_msg}")
        self.is_connected = False
    
    def get_status(self):
        """Get current scanner status"""
        return {
            'is_running': self.is_running,
            'is_connected': self.is_connected,
            'tracked_tokens': len(self.reserved_seats_map),
            'tracked_seats': sum(len(seats) for seats in self.reserved_seats_map.values())
        }


class BotGUI:
    """Main GUI application"""
    
    def __init__(self, root):
        self.root = root
        self.root.title("Webook Bot - Browser Automation")
        self.root.geometry("1300x700")
        
        # Data storage
        self.users = []
        self.proxies = []
        self.workers = []
        self.channels = []
        self.seasonStructure = None
        self.displayChannels = False
        self.stop_event = threading.Event()
        self.browser_semaphore = threading.Semaphore(1)  # Only 1 browser at a time
        self.countdown_timer = None
        self.init_barrier = None 
        self.start_countdown_timer()
        self.seat_scanner = SeatScanner()
        #self.seat_scanner.set_log_callback()
        self.scanner_started = False
        
        # UI queue for thread-safe updates
        self.ui_queue = queue.Queue()
        
        # Settings
        self.headless_var = tk.BooleanVar(value=False)
        
        self._create_widgets()
        self._start_ui_updater()
    
    def _create_widgets(self):
        """Create GUI widgets"""
        # Top frame for controls
        top_frame = ttk.Frame(self.root)
        top_frame.pack(fill='x', padx=10, pady=5)
        
        # File upload buttons
        ttk.Button(top_frame, text="Load Users CSV", command=self.load_users).grid(row=0, column=0, padx=5)
        ttk.Button(top_frame, text="Load Proxies", command=self.load_proxies).grid(row=0, column=1, padx=5)
        
        # Event URL input
        ttk.Label(top_frame, text="Event URL:").grid(row=0, column=2, padx=(20,5))
        self.event_url_var = tk.StringVar()
        event_entry = ttk.Entry(top_frame, textvariable=self.event_url_var, width=50)
        event_entry.grid(row=0, column=3, padx=5)
        
        # Seats input
        ttk.Label(top_frame, text="Seats:").grid(row=0, column=4, padx=(10,5))
        self.seats_var = tk.StringVar(value="1")
        seats_entry = ttk.Entry(top_frame, textvariable=self.seats_var, width=5)
        seats_entry.grid(row=0, column=5, padx=5)
        
        # Options
        ttk.Label(top_frame, text="DSeats:").grid(row=0, column=6, padx=(10,5))
        self.Dseats_var = tk.StringVar(value="1")
        Dseats_entry = ttk.Entry(top_frame, textvariable=self.Dseats_var, width=5)
        Dseats_entry.grid(row=0, column=7, padx=5)
        
        # Control buttons
        self.start_btn = ttk.Button(top_frame, text="Start Bot", command=self.start_bot)
        self.start_btn.grid(row=0, column=8, padx=5)
        
        self.stop_btn = ttk.Button(top_frame, text="Stop Bot", command=self.stop_bot, state='disabled')
        self.stop_btn.grid(row=0, column=9, padx=5)
        self.fill_seats_btn = ttk.Button(top_frame, text="Fill Seats", command=self.fill_seats, state='disabled')
        self.fill_seats_btn.grid(row=0, column=10, padx=5)

        self.send_seat_btn = ttk.Button(top_frame, text="Send Seat", command=self.open_send_seat_dialog)
        self.send_seat_btn.grid(row=0, column=11, padx=5)
        # Create scrollable checkbox container
        checkbox_container = ttk.Frame(self.root)
        checkbox_container.pack(fill='x', padx=12, pady=5)

        # Canvas for scrolling
        self.checkbox_canvas = tk.Canvas(checkbox_container, height=40)
        self.checkbox_scrollbar = ttk.Scrollbar(checkbox_container, orient='horizontal', command=self.checkbox_canvas.xview)
        self.checkbox_canvas.configure(xscrollcommand=self.checkbox_scrollbar.set)

        # Inner frame for checkboxes
        self.checkbox_frame = ttk.Frame(self.checkbox_canvas)
        self.checkbox_canvas_window = self.checkbox_canvas.create_window(0, 0, anchor='nw', window=self.checkbox_frame)

        # Pack canvas and scrollbar
        self.checkbox_canvas.pack(side='top', fill='x')
        self.checkbox_scrollbar.pack(side='bottom', fill='x')

        # Users table
        self._create_users_table()
        
        # Status bar
        self.status_var = tk.StringVar(value="Ready - Load users and proxies to begin")
        status_bar = ttk.Label(self.root, textvariable=self.status_var, relief='sunken')
        status_bar.pack(side='bottom', fill='x', padx=10, pady=5)
    
    def open_send_seat_dialog(self):
        """Open dialog for transferring seats between accounts"""
        # Create modal dialog
        dialog = tk.Toplevel(self.root)
        dialog.title("Transfer Seat")
        dialog.geometry("400x200")
        dialog.transient(self.root)
        dialog.grab_set()
        
        # Get users with seats
        star_users = [(i, user) for i, user in enumerate(self.users) 
              if user.get('type') == '*']
        plus_users = [(i, user) for i, user in enumerate(self.users) 
              if user.get('type') == '+']
        
        # From account dropdown
        ttk.Label(dialog, text="From Account:").grid(row=0, column=0, padx=10, pady=10, sticky='w')
        from_var = tk.StringVar()
        from_combo = ttk.Combobox(dialog, textvariable=from_var, width=30)
        from_combo['values'] = [f"{user['email']} ({user['seats_booked']} seats)" 
                        for i, user in star_users]
        from_combo.grid(row=0, column=1, padx=10, pady=10)
        
        # To account dropdown
        ttk.Label(dialog, text="To Account:").grid(row=1, column=0, padx=10, pady=10, sticky='w')
        to_var = tk.StringVar()
        to_combo = ttk.Combobox(dialog, textvariable=to_var, width=30)
        to_combo['values'] = [user['email'] for i, user in plus_users]
        to_combo.grid(row=1, column=1, padx=10, pady=10)
        
        # Buttons
        button_frame = ttk.Frame(dialog)
        button_frame.grid(row=2, column=0, columnspan=2, pady=20)
        
        def transfer_seat():
            from_email = from_var.get().split(' (')[0] if from_var.get() else None
            to_email = to_var.get()
            
            if from_email and to_email and from_email != to_email:
                # Find user indices
                from_idx = next((i for i, u in enumerate(self.users) 
                                if u['email'] == from_email), None)
                to_idx = next((i for i, u in enumerate(self.users) 
                            if u['email'] == to_email), None)
                
                if from_idx is not None and to_idx is not None:
                    dialog.destroy()
                    # Start transfer in background thread
                    threading.Thread(
                        target=self._transfer_seat,
                        args=(from_idx, to_idx),
                        daemon=True
                    ).start()
            else:
                messagebox.showwarning("Invalid Selection", "Please select different accounts")
        
        ttk.Button(button_frame, text="Transfer", command=transfer_seat).pack(side='left', padx=5)
        ttk.Button(button_frame, text="Cancel", command=dialog.destroy).pack(side='left', padx=5)
    def _transfer_seat(self, from_idx, to_idx):
        """Transfer seat(s) from one user to another based on Dseats_var"""
        from_user = self.users[from_idx]
        to_user = self.users[to_idx]
        
        from_bot = from_user.get('bot_instance')
        to_bot = to_user.get('bot_instance')
        
        if not from_bot or not to_bot:
            return
        
        # Get number of seats to transfer from Dseats_var
        try:
            dseats_var = getattr(self, 'Dseats_var', None)
            print(f'dseats_var {dseats_var}')
            if dseats_var and hasattr(dseats_var, 'get'):
                # It's a StringVar or similar, get its value
                seats_to_transfer_count = int(dseats_var.get())
            else:
                # It's already a value or doesn't exist
                seats_to_transfer_count = int(dseats_var) if dseats_var else 1
        except (ValueError, AttributeError):
            # If conversion fails, default to 1
            seats_to_transfer_count = 1
        
        print(f'seats_to_transfer_count {seats_to_transfer_count}')
        def log_from(msg):
            timestamp = datetime.now().strftime('%H:%M:%S')
            from_user['logs'].append(f"[{timestamp}] {msg}")
        
        def log_to(msg):
            timestamp = datetime.now().strftime('%H:%M:%S')
            to_user['logs'].append(f"[{timestamp}] {msg}")
        
        # Update initial statuses
        self.ui_queue.put(('update_user', from_idx, 'status', f'Releasing {seats_to_transfer_count} seat(s)...'))
        self.ui_queue.put(('update_user', to_idx, 'status', f'Waiting for {seats_to_transfer_count} seat(s)...'))
        
        transferred_seats = []
        failed_transfers = []
        
        try:
            # Transfer each seat
            for i in range(seats_to_transfer_count):
                if not from_user.get('assigned_seats'):
                    break
                    
                seat_to_transfer = from_user['assigned_seats'][0]  # Always take the first available seat
                
                log_from(f"📤 Releasing seat {seat_to_transfer} ({i+1}/{seats_to_transfer_count}) for transfer...")
                
                # Step 1: Release seat from sender
                release_result = from_bot.releaseSeat(
                    seat_to_transfer,
                    from_bot.webook_hold_token,
                    log_callback=log_from
                )
                
                if release_result.get('success'):
                    # Update sender's seats
                    from_user['seats_booked'] -= 1
                    from_user['assigned_seats'].remove(seat_to_transfer)
                    self.ui_queue.put(('update_user', from_idx, 'seats_booked', from_user['seats_booked']))
                    log_from(f"✓ Seat {seat_to_transfer} released ({i+1}/{seats_to_transfer_count})")
                    
                    # Step 2: Take seat with receiver
                    log_to(f"📥 Taking transferred seat {seat_to_transfer} ({i+1}/{seats_to_transfer_count})...")
                    take_result = to_bot.takeSeat(
                        seat_to_transfer,
                        log_callback=log_to
                    )
                    
                    if take_result.get('success'):
                        # Update receiver's seats
                        to_user['seats_booked'] = to_user.get('seats_booked', 0) + 1
                        if 'assigned_seats' not in to_user:
                            to_user['assigned_seats'] = []
                        to_user['assigned_seats'].append(seat_to_transfer)
                        self.ui_queue.put(('update_user', to_idx, 'seats_booked', to_user['seats_booked']))
                        log_to(f"✓ Seat {seat_to_transfer} acquired ({i+1}/{seats_to_transfer_count})")
                        transferred_seats.append(seat_to_transfer)
                    else:
                        log_to(f"✗ Failed to take seat {seat_to_transfer} ({i+1}/{seats_to_transfer_count})")
                        failed_transfers.append(seat_to_transfer)
                else:
                    log_from(f"✗ Failed to release seat {seat_to_transfer} ({i+1}/{seats_to_transfer_count})")
                    failed_transfers.append(seat_to_transfer)
                    break  # Stop trying if release fails
            
            # Update final statuses
            if len(transferred_seats) == seats_to_transfer_count:
                self.ui_queue.put(('update_user', from_idx, 'status', f'Transfer complete ({len(transferred_seats)} seats)'))
                self.ui_queue.put(('update_user', to_idx, 'status', f'Transfer complete ({len(transferred_seats)} seats)'))
                log_from(f"✓ Successfully transferred {len(transferred_seats)} seat(s)")
                log_to(f"✓ Successfully received {len(transferred_seats)} seat(s)")
            elif len(transferred_seats) > 0:
                self.ui_queue.put(('update_user', from_idx, 'status', f'Partial transfer ({len(transferred_seats)}/{seats_to_transfer_count})'))
                self.ui_queue.put(('update_user', to_idx, 'status', f'Partial transfer ({len(transferred_seats)}/{seats_to_transfer_count})'))
                log_from(f"⚠ Partial transfer: {len(transferred_seats)}/{seats_to_transfer_count} seats")
                log_to(f"⚠ Partial transfer: {len(transferred_seats)}/{seats_to_transfer_count} seats")
            else:
                self.ui_queue.put(('update_user', from_idx, 'status', 'Transfer failed'))
                self.ui_queue.put(('update_user', to_idx, 'status', 'Transfer failed'))
                log_from(f"✗ Transfer failed: 0/{seats_to_transfer_count} seats transferred")
                log_to(f"✗ Transfer failed: 0/{seats_to_transfer_count} seats received")
                
        except Exception as e:
            log_from(f"✗ Transfer error: {str(e)}")
            log_to(f"✗ Transfer error: {str(e)}")
            self.ui_queue.put(('update_user', from_idx, 'status', 'Transfer error'))
            self.ui_queue.put(('update_user', to_idx, 'status', 'Transfer error'))
    def start_countdown_timer(self):
        """Countdown timer for expire times"""
        def update_countdowns():
            for i, user in enumerate(self.users):
                if user.get('expireTime', 0) > 0:
                    user['expireTime'] -= 1
                    self.ui_queue.put(('update_user', i, 'expireTime', user['expireTime']))
                    
                    # When reaches 0, regenerate hold token
                    if user['expireTime'] == 0 and user.get('bot_instance'):
                        user['assigned_seats'] = []
                        user['seats_booked'] = 0
                        self.ui_queue.put(('update_user', i, 'seats_booked', 0))
                        def regenerate_and_retry():
                            success = self.regenerate_hold_token(i, user)
                            # ONLY retry seats if token regeneration was successful
                            if success and user.get('save_seat_assignment') and user.get("type") == "*":
                                self._take_seats_for_user(i, user, user['save_seat_assignment'])
                        
                        threading.Thread(
                            target=regenerate_and_retry,
                            daemon=True
                        ).start()
            
            self.countdown_timer = self.root.after(1000, update_countdowns)
        
        update_countdowns()

    def regenerate_hold_token(self, user_index, user):
        """Regenerate hold token when expired"""
        bot = user.get('bot_instance')
        if not bot:
            return
        
        def log(msg):
            timestamp = datetime.now().strftime('%H:%M:%S')
            log_entry = f"[{timestamp}] {msg}"
            user['logs'].append(log_entry)
        
        try:
            log("⏰ Hold token expired, regenerating...")
            bot.getWebookHoldToken(log_callback=log)
            bot.getExpireTime(log_callback=log)
            user['expireTime'] = bot.expireTime
            self.ui_queue.put(('update_user', user_index, 'expireTime', bot.expireTime))
            log(f"✓ New hold token generated, expires in {bot.expireTime} seconds")
            return True
        except Exception as e:
            log(f"🔄 Retrying token generation ...")
            time.sleep(2)  # Wait 2 seconds before retry
            return self.regenerate_hold_token(user_index, user)
    def _create_checkBox(self): 
        """Create checkboxes for channel selection in one line"""
        if not hasattr(self, 'channels') or not self.channels:
            return
            
        # Clear existing checkboxes first
        for widget in self.checkbox_frame.winfo_children():
            widget.destroy()
            
        # Extract unique prefixes from channels
        unique_prefixes = set()
        for channel in self.channels:
            if isinstance(channel, str) and '-' in channel:
                prefix = channel.split('-')[0]
                unique_prefixes.add(prefix)
        
        if not unique_prefixes:
            return
            
        ttk.Label(self.checkbox_frame, text="Sections:").pack(side='left', padx=(0,10))
        
        # Create checkboxes in one line
        for prefix in sorted(unique_prefixes):
            var = tk.BooleanVar(value=False)
            checkbox = ttk.Checkbutton(self.checkbox_frame, text=f"{prefix}", variable=var)
            checkbox.pack(side='left', padx=5)
            setattr(self, f"{prefix}_var", var)
        # Update scroll region after adding checkboxes
        self.checkbox_frame.update_idletasks()
        self.checkbox_canvas.configure(scrollregion=self.checkbox_canvas.bbox('all'))
        self.displayChannels = True
    def _create_users_table(self):
        """Create the users display table"""
        table_frame = ttk.Frame(self.root)
        table_frame.pack(fill='both', expand=True, padx=10, pady=10)
        
        # Treeview for users
        columns = ('Email', 'Type', 'Proxy', 'Status', 'Expire Time', 'Seats Booked', 'Last Update')
        self.tree = ttk.Treeview(table_frame, columns=columns, show='headings', selectmode='browse')
        
        # Configure columns
        for col in columns:
            self.tree.heading(col, text=col)
            if col == 'Email':
                self.tree.column(col, width=250)
            elif col == 'Proxy':
                self.tree.column(col, width=200)
            elif col == 'Status':
                self.tree.column(col, width=150)
            elif col == 'Expire Time':
                self.tree.column(col, width=100, anchor='center')
            elif col in ['Seats Booked', 'Type']:
                self.tree.column(col, width=100, anchor='center')
            else:
                self.tree.column(col, width=150)
        
        # Scrollbars
        v_scrollbar = ttk.Scrollbar(table_frame, orient='vertical', command=self.tree.yview)
        self.tree.configure(yscrollcommand=v_scrollbar.set)
        
        h_scrollbar = ttk.Scrollbar(table_frame, orient='horizontal', command=self.tree.xview)
        self.tree.configure(xscrollcommand=h_scrollbar.set)
        
        # Pack widgets
        self.tree.grid(row=0, column=0, sticky='nsew')
        v_scrollbar.grid(row=0, column=1, sticky='ns')
        h_scrollbar.grid(row=1, column=0, sticky='ew')
        
        table_frame.grid_rowconfigure(0, weight=1)
        table_frame.grid_columnconfigure(0, weight=1)
        
        # Double-click to view logs
        self.tree.bind('<Double-1>', self.show_user_logs)
    
    def load_users(self):
        """Load users from CSV file"""
        file_path = filedialog.askopenfilename(
            title="Select Users CSV",
            filetypes=[("CSV files", "*.csv"), ("All files", "*.*")]
        )
        
        if not file_path:
            return
        
        try:
            with open(file_path, 'r', newline='', encoding='utf-8') as csvfile:
                reader = csv.DictReader(csvfile)
                users_data = list(reader)
            
            if not users_data:
                messagebox.showerror("Error", "CSV file is empty")
                return
            
            # Validate required columns
            required_columns = {'email', 'password', 'type'}
            if not required_columns.issubset({col.lower() for col in reader.fieldnames}):
                messagebox.showerror("Error", f"CSV must contain columns: {', '.join(required_columns)}")
                return
            
            # Process users data
            self.users = []
            for row in users_data:
                user = {
                    'email': row.get('email', '').strip(),
                    'password': row.get('password', '').strip(),
                    'type': row.get('type', '*').strip(),
                    'proxy': None,
                    'status': 'Ready',
                    'expireTime': 0,
                    'seats_booked': 0,
                    'last_update': datetime.now().strftime('%H:%M:%S'),
                    'logs': []
                }
                
                if user['email'] and user['password']:
                    self.users.append(user)
            
            # Assign proxies
            self._assign_proxies()
            self._refresh_users_table()
            
            self.status_var.set(f"Loaded {len(self.users)} users")
            messagebox.showinfo("Success", f"Loaded {len(self.users)} users from CSV")
            
        except Exception as e:
            messagebox.showerror("Error", f"Failed to load users: {str(e)}")
    
    def load_proxies(self):
        """Load proxies from text file"""
        file_path = filedialog.askopenfilename(
            title="Select Proxies File",
            filetypes=[("Text files", "*.txt"), ("All files", "*.*")]
        )
        
        if not file_path:
            return
        
        try:
            with open(file_path, 'r', encoding='utf-8') as f:
                self.proxies = [line.strip() for line in f if line.strip()]
            
            self._assign_proxies()
            self._refresh_users_table()
            
            messagebox.showinfo("Success", f"Loaded {len(self.proxies)} proxies")
            self.status_var.set(f"Loaded {len(self.proxies)} proxies")
            
        except Exception as e:
            messagebox.showerror("Error", f"Failed to load proxies: {str(e)}")
    
    def _assign_proxies(self):
        """Assign proxies to users in round-robin fashion"""
        if not self.proxies or not self.users:
            return
        
        for i, user in enumerate(self.users):
            user['proxy'] = self.proxies[i % len(self.proxies)]
    
    def _refresh_users_table(self):
        """Refresh the users table display"""
        # Clear existing items
        for item in self.tree.get_children():
            self.tree.delete(item)
        
        # Add users
        for i, user in enumerate(self.users):
            values = (
                user['email'],
                user['type'],
                user.get('proxy', 'None'),
                user['status'],
                str(user.get('expireTime', 0)),
                str(user['seats_booked']),
                user['last_update']
            )
            self.tree.insert('', 'end', iid=str(i), values=values)
    
    def start_bot(self):
        """Start the bot operations"""
        if not self.users:
            messagebox.showwarning("No Users", "Please load users first")
            return
        
        if not PLAYWRIGHT_AVAILABLE:
            messagebox.showerror("Missing Dependency", 
                               "Playwright is not installed.\n\nInstall with:\n"
                               "pip install playwright\n"
                               "python -m playwright install chromium")
            return
        
        event_url = self.event_url_var.get().strip()
        if not event_url:
            messagebox.showwarning("Missing URL", "Please enter the event URL")
            return
        
        try:
            seats_count = int(self.seats_var.get())
            dSeats_count = int(self.Dseats_var.get())
            if seats_count < 1 or dSeats_count < 1:
                raise ValueError()
        except ValueError:
            messagebox.showerror("Invalid Seats", "Please enter a valid number of seats")
            return
        
        # Update UI state
        self.start_btn.config(state='disabled')
        self.stop_btn.config(state='normal')
        self.stop_event.clear()
        
        # Start worker threads
        self.workers = []
        self.init_barrier = threading.Barrier(len(self.users))  # Add this line
        for i, user in enumerate(self.users):
            worker = threading.Thread(
                target=self._worker_thread,
                args=(i, user, event_url, seats_count, dSeats_count),
                daemon=True
            )
            self.workers.append(worker)
            worker.start()
        
        self.status_var.set(f"Bot started with {len(self.workers)} workers")
    
    def stop_bot(self):
        """Stop the bot operations"""
        self.stop_event.set()
        self.start_btn.config(state='normal')
        self.fill_seats_btn.config(state='disabled')
        self.stop_btn.config(state='disabled')
        self.status_var.set("Stopping bot...")
    
    def _worker_thread(self, user_index, user, event_url, seats_count, dSeats_count):
        """Worker thread for individual user operations"""
        def log(message):
            timestamp = datetime.now().strftime('%H:%M:%S')
            log_entry = f"[{timestamp}] {message}"
            user['logs'].append(log_entry)
            user['last_update'] = timestamp
            self.ui_queue.put(('update_user', user_index, 'last_update', timestamp))
        
        def update_status(status):
            user['status'] = status
            self.ui_queue.put(('update_user', user_index, 'status', status))
        
        try:
            # BROWSER OPERATIONS - ONE AT A TIME (only for login)
            with self.browser_semaphore:  # Only one browser at a time for login
                update_status("Waiting in queue...")
                log("Waiting for browser availability...")
                
                bot = WebookBot(
                    email=user['email'],
                    password=user['password'],
                    headless=self.headless_var.get(),
                    proxy=user.get('proxy')
                )
                
                # Store bot instance in user dict
                user['bot_instance'] = bot
                
                # Step 1: Login and get tokens
                update_status("Logging in...")
                log(f"Starting login process for {user['email']}")
                
                try:
                    token_data = bot.login_with_browser(
                        event_url,
                        log_callback=log
                    )
                    
                    if not token_data:
                        raise Exception("Login failed - no tokens received")
                    
                    update_status("Login successful")
                    log("✓ Login completed successfully")
                    
                except Exception as e:
                    update_status("Login failed")
                    log(f"✗ Login failed: {str(e)}")
                    return
            
            # AFTER LOGIN - ALL THREADS WORK IN PARALLEL
            # No semaphore here - all threads can work simultaneously
            update_status("Initializing seat system...")
            
            try:
                bot.extract_seatsio_chart_data(log_callback=log)
                path_parts = urlparse(event_url).path.strip("/").split("/")
                event_id = path_parts[2]
                bot.send_event_detail_request(event_id, log_callback=log)
                self.channels, self.seasonStructure = bot.get_rendering_info(log_callback=log)
                #print(self.channels)
                
                if not self.displayChannels and user_index == 0:
                    self._create_checkBox()
                
                # Get hold token and expire time
                bot.getWebookHoldToken(log_callback=log)
                bot.getExpireTime(log_callback=log)
                
                # Update expire time in user dict and UI
                user['expireTime'] = bot.expireTime
                self.ui_queue.put(('update_user', user_index, 'expireTime', bot.expireTime))
                
                update_status("Ready for booking")
                log(f"✓ System ready, hold token expires in {bot.expireTime} seconds")
                # Enable fill seats button when first user is ready
                if user_index == 0:
                    self.fill_seats_btn.config(state='normal')
                self.init_barrier.wait()
                update_status("All users ready - starting operations")
                log("All users initialized, starting synchronized operations")
                
                # Now you can add seat booking logic here
                # All threads will execute this part in parallel
                # Every user updates the scanner with their info
                # Start scanner once with all users
                if user_index == 0 and not self.scanner_started:
                    try:
                        self.seat_scanner.set_event_key(bot.event_key, bot.seasonStructure)
                        self.seat_scanner.set_users(self.users)  # All users, scanner will filter by type *
                        self.seat_scanner.start_scanning()
                        self.scanner_started = True
                        log("🔍 Auto-started seat scanner for all type * users")
                        self.status_var.set("Bot ready - Scanner monitoring seats...")
                    except Exception as e:
                        log(f"Failed to start scanner: {str(e)}")

                # Always update scanner with current user list after all users are ready
                if user_index == len(self.users) - 1:  # Last user updates the complete list
                    self.seat_scanner.set_users(self.users)
                    log(f"🔍 Scanner updated with {len(self.users)} users")
                
            except Exception as e:
                update_status("Initialization failed")
                log(f"✗ Failed to initialize: {str(e)}")
                
        except Exception as e:
            update_status("Error")
            log(f"✗ Worker error: {str(e)}")
            traceback.print_exc()
        
        finally:
            if not self.stop_event.is_set():
                update_status("Completed")
                log("Worker thread completed")
    def show_user_logs(self, event):
        """Show detailed logs for selected user"""
        selection = self.tree.selection()
        if not selection:
            return
        
        user_index = int(selection[0])
        user = self.users[user_index]
        
        # Create log window
        log_window = tk.Toplevel(self.root)
        log_window.title(f"Logs - {user['email']}")
        log_window.geometry("800x600")
        
        # Text widget with scrollbar
        frame = ttk.Frame(log_window)
        frame.pack(fill='both', expand=True, padx=10, pady=10)
        
        text_widget = tk.Text(frame, wrap='word', font=('Courier', 10))
        scrollbar = ttk.Scrollbar(frame, orient='vertical', command=text_widget.yview)
        text_widget.configure(yscrollcommand=scrollbar.set)
        
        # Display logs
        for log_entry in user['logs']:
            text_widget.insert('end', log_entry + '\n')
        
        text_widget.config(state='disabled')
        
        text_widget.pack(side='left', fill='both', expand=True)
        scrollbar.pack(side='right', fill='y')
        
        # Auto-refresh logs
        def refresh_logs():
            if log_window.winfo_exists():
                text_widget.config(state='normal')
                text_widget.delete('1.0', 'end')
                for log_entry in user['logs']:
                    text_widget.insert('end', log_entry + '\n')
                text_widget.config(state='disabled')
                text_widget.see('end')
                log_window.after(1000, refresh_logs)
        
        refresh_logs()
    
    def _start_ui_updater(self):
        """Start the UI update loop"""
        def process_ui_queue():
            try:
                while True:
                    try:
                        command, *args = self.ui_queue.get_nowait()
                        
                        if command == 'update_user':
                            user_index, field, value = args
                            item_id = str(user_index)
                            if self.tree.exists(item_id):
                                values = list(self.tree.item(item_id, 'values'))
                                if field == 'status':
                                    values[3] = value
                                elif field == 'expireTime':
                                    values[4] = str(value)
                                elif field == 'seats_booked':
                                    values[5] = str(value)
                                elif field == 'last_update':
                                    values[6] = value
                                self.tree.item(item_id, values=values)
                        
                    except queue.Empty:
                        break
                    except Exception as e:
                        print(f"UI update error: {e}")
                        
            finally:
                self.root.after(100, process_ui_queue)
        
        process_ui_queue()

    def fill_seats(self):
        """Distribute and take seats"""
        print(f"Fill seats called - Channels: {len(self.channels)}")
        print(f"Selected sections check...")

        seats_per_user = int(self.seats_var.get())
        
        # Get selected sections from checkboxes
        selected_sections = []
        for channel in self.channels:
            if isinstance(channel, str) and '-' in channel:
                prefix = channel.split('-')[0]
                if hasattr(self, f"{prefix}_var") and getattr(self, f"{prefix}_var").get():
                    selected_sections.append(prefix)
        
        # Filter channels by selected sections
        available_seats = [ch for ch in self.channels 
                        if any(ch.startswith(prefix) for prefix in selected_sections)]
        
        # Group users by type
        users_by_type = {}
        for user in self.users:
            user_type = user.get('type', '*')
            if user_type not in users_by_type:
                users_by_type[user_type] = []
            users_by_type[user_type].append(user)
        #print(users_by_type)
        # Distribute seats
        seat_assignments = {}
        seat_index = 0
        star_users = users_by_type.get('*', [])
        #for user_type, type_users in users_by_type.items():
        for user in star_users:
            user_seats = []
            for _ in range(min(seats_per_user, len(available_seats) - seat_index)):
                if seat_index < len(available_seats):
                    user_seats.append(available_seats[seat_index])
                    seat_index += 1
            seat_assignments[user['email']] = user_seats
        
        # Start parallel seat taking
        threading.Thread(target=self._execute_seat_taking, args=(seat_assignments,), daemon=True).start()

    def _execute_seat_taking(self, seat_assignments):
        """Execute parallel seat taking"""
        threads = []
        
        for i, user in enumerate(self.users):
            # Only process users with type '*'
            if user.get('type') == '*':
                seats = seat_assignments.get(user['email'], [])
                user['assigned_seats'] = seats
                user['save_seat_assignment'] = seats
                if seats and user.get('bot_instance'):
                    thread = threading.Thread(
                        target=self._take_seats_for_user,
                        args=(i, user, seats),
                        daemon=True
                    )
                    threads.append(thread)
                    thread.start()
        
        # Wait for all to complete
        for thread in threads:
            thread.join()

    def _take_seats_for_user(self, user_index, user, seats):
        """Take seats for individual user"""
        bot = user.get('bot_instance')
        if not bot:
            return
        
        def log(msg):
            timestamp = datetime.now().strftime('%H:%M:%S')
            log_entry = f"[{timestamp}] {msg}"
            user['logs'].append(log_entry)
        
        self.ui_queue.put(('update_user', user_index, 'status', 'Taking seats...'))
        
        taken_count = 0
        #user['seats_booked'] = 0
        #self._refresh_users_table()
        
        # Send all requests without waiting
        with concurrent.futures.ThreadPoolExecutor(max_workers=50) as executor:
            # Submit all requests immediately
            futures = {}
            for seat in seats:
                future = executor.submit(bot.takeSeat, seat, log_callback=log)
                futures[future] = seat
                log(f"→ Request sent for seat {seat}")
            
            # Process responses as they arrive (FIFO)
            for future in concurrent.futures.as_completed(futures):
                seat = futures[future]
                try:
                    result = future.result(timeout=30)  # Don't wait long
                    if result.get('success'):
                        taken_count += 1
                        user['seats_booked'] = taken_count
                        self.ui_queue.put(('update_user', user_index, 'seats_booked', taken_count))
                        log(f"✓ Seat {seat} taken successfully")
                    else:
                        log(f"✗ Failed to take seat {seat}")
                except Exception as e:
                    log(f"✗ Error for seat {seat}: {str(e)}")
        
        self.ui_queue.put(('update_user', user_index, 'status', f'Completed - {taken_count} seats'))
def main():
    """Main application entry point"""
    root = tk.Tk()
    app = BotGUI(root)
    root.mainloop()

if __name__ == "__main__":
    main()
