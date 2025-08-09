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
import queue
import csv
import time
import json
import os
import requests
from pathlib import Path
from datetime import datetime, timedelta
import traceback
import random
import secrets
import re
import hashlib
from urllib.parse import urljoin, urlparse


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
        self.browser_id = None
        self.channels = None
        
        # Setup session defaults
        #self.session.headers.update({
        #    'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
        #    'Accept': 'application/json, text/plain, */*',
        #    'Accept-Language': 'en-US,en;q=0.9',
        #    'Content-Type': 'application/json',
        #})
    
    def _setup_proxy(self, session_or_context, proxy_str):
        """Setup proxy for requests session or playwright context"""
        if not proxy_str:
            return
        
        if isinstance(session_or_context, requests.Session):
            # Format: http://user:pass@host:port or http://host:port
            session_or_context.proxies = {
                'http': proxy_str,
                'https': proxy_str
            }
        else:
            # Playwright context
            return {'server': proxy_str}
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
            response = requests.get(url, headers=headers, timeout=30)
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
            raise Exception(error_msg)
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
                self.event_key =response_data['data']['seats_io']['event_key']
                print(self.chart_key)
                print(self.event_key)
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
            
            response = requests.get(url, headers=headers, params=params, timeout=30)
            response.raise_for_status()
            
            data = response.json()
            log(f"✓ Successfully fetched rendering info")
            #print(data)
            all_objects = []
            for channel in data.get('channels', []):
                if 'objects' in channel:
                    all_objects.extend(channel['objects'])
            self.channels = all_objects
            return self.channels
            
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
        
        # Generate signature using HMAC-SHA256 with chartToken as key
        import hmac
        
        # The signature appears to be HMAC-SHA256(chartToken, request_body)
        signature = hmac.new(
            self.chartToken.encode('utf-8'),
            request_body.encode('utf-8'),
            hashlib.sha256
        ).hexdigest()
        
        return signature
    def login_with_browser(self, event_url="", log_callback=None):
        """Login using browser automation and capture tokens"""
        if not PLAYWRIGHT_AVAILABLE:
            raise Exception("Playwright not installed. Run: pip install playwright && python -m playwright install chromium")
        
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        log(f"Starting browser login for {self.email}")
        
        # Check if we already have valid tokens
        existing_token = self.token_manager.get_valid_token(self.email)
        if existing_token:
            log(f"Using existing valid token for {self.email}")
            return existing_token
        
        proxy_config = None
        if self.proxy:
            proxy_config = self._setup_proxy(None, self.proxy)
        
        log("Opening playwright browser")
        with sync_playwright() as p:
            browser = p.chromium.launch(
                headless=self.headless,
                args=['--no-sandbox', '--disable-dev-shm-usage']
            )
            
            context = browser.new_context(
                proxy=proxy_config,
                viewport={'width': 1920, 'height': 1080},
                user_agent='Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
            )
            
            page = context.new_page()
            
            # Intercept API responses to capture tokens
            captured_tokens = {}
            
            def handle_response(response):
                try:
                    if 'api.webook.com/api/v2/login' in response.url and response.status == 200:
                        data = response.json()
                        if data.get('status') == 'success' and 'data' in data:
                            captured_tokens.update(data['data'])
                            log("✓ Login tokens captured from API response")
                except Exception as e:
                    log(f"Error parsing response: {e}")
            def handle_request(request):
                try:
                    if 'api.webook.com/api/v2/login' in request.url:
                        headers = request.headers
                        token = headers.get('token')
                        captured_tokens['token'] = token
                        log("✓ Login tokens captured from API request")
                except Exception as e:
                    log(f"Error parsing response: {e}")
            
            page.on('response', handle_response)
            page.on('request', handle_request)
            
            try:
                # Navigate to login page with redirect to event URL if provided
                if event_url:
                    # Extract the path from event URL for redirect
                    from urllib.parse import urlparse
                    parsed_url = urlparse(event_url)
                    redirect_path = parsed_url.path
                    login_url = f"https://webook.com/en/login?redirect=%2Fen"
                else:
                    login_url = "https://webook.com/en/login?redirect=%2Fen"
                
                log(f"Navigating to {login_url}")
                page.goto(login_url, timeout=CONFIG['navigation_timeout'])
                
                # Handle cookie consent banner
                log("Handling cookie consent")
                self._handle_cookie_consent(page, log)
                
                # Wait for login form to be visible
                log("Waiting for login form")
                page.wait_for_selector('form[name="email-login-form"]', timeout=10000)
                log("Login form found")
                
                # Fill login credentials using the specific selectors
                log("Filling login credentials")
                email_input = page.locator('input[data-testid="auth_login_email_input"]')
                password_input = page.locator('input[data-testid="auth_login_password_input"]')
                login_button = page.locator('button[data-testid="auth_login_submit_button"]')
                
                # Clear and fill email
                email_input.fill("")
                email_input.fill(self.email)
                
                # Clear and fill password
                password_input.fill("")
                password_input.fill(self.password)
                
                log("Credentials filled, submitting form")
                
                # Submit login form and wait for API response
                with page.expect_response(
                    lambda r: 'api.webook.com/api/v2/login' in r.url, 
                    timeout=CONFIG['login_timeout']
                ) as response_info:
                    login_button.click()
                    log("Login button clicked, waiting for response...")
                
                response = response_info.value
                log(f"Received response with status: {response.status}")
                
                if response.status == 200:
                    response_data = response.json()
                    log(f"Response data status: {response_data.get('status')}")
                    
                    if response_data.get('status') == 'success':
                        log("✓ Login successful")
                    else:
                        error_msg = response_data.get('message', 'Unknown login error')
                        raise Exception(f"Login failed: {error_msg}")
                else:
                    raise Exception(f"Login failed with status {response.status}")
                
                # Wait a moment for tokens to be captured
                page.wait_for_timeout(2000)
                
                '''if not captured_tokens.get('access_token'):
                    # Try to extract tokens from local storage or cookies as fallback
                    log("No tokens in API response, checking browser storage...")
                    try:
                        # Check localStorage
                        local_storage = page.evaluate("() => Object.assign({}, localStorage)")
                        log(f"LocalStorage contents: {list(local_storage.keys())}")
                        
                        # Look for token-related keys
                        for key, value in local_storage.items():
                            if 'token' in key.lower() or 'auth' in key.lower():
                                try:
                                    token_data = json.loads(value)
                                    if isinstance(token_data, dict) and 'access_token' in token_data:
                                        captured_tokens.update(token_data)
                                        log("✓ Tokens found in localStorage")
                                        break
                                except:
                                    continue
                    except Exception as e:
                        log(f"Error checking browser storage: {e}")
                
                if not captured_tokens.get('access_token'):
                    raise Exception("No access token found in response or browser storage")'''
                #print(captured_tokens)
                # Store tokens
                self.token_manager.store_token(self.email, captured_tokens)
                log(f"✓ Tokens stored for {self.email}")
                
                return self.token_manager.get_valid_token(self.email)
                
            except PlaywrightTimeoutError as e:
                raise Exception(f"Login timeout - page took too long to respond: {str(e)}")
            except Exception as e:
                log(f"Login error: {str(e)}")
                raise
            finally:
                browser.close()
    
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
    
    def book_seat(self, email, event_url, seats_count=1, log_callback=None):
        """Book seats for an event"""
        def log(msg):
            if log_callback:
                log_callback(msg)
            print(msg)
        
        # Get valid token
        token_data = self.token_manager.get_valid_token(email)
        if not token_data:
            raise Exception(f"No valid token for {email}. Login required.")
        
        access_token = token_data['access_token']
        
        # Setup request headers
        headers = {
            'Authorization': f'Bearer {access_token}',
            'Content-Type': 'application/json',
            'Referer': event_url,
            'Origin': 'https://webook.com'
        }
        
        # Extract event ID from URL (placeholder - adjust based on actual URL format)
        event_id = self._extract_event_id(event_url)
        
        # Prepare booking payload (this is a placeholder - adjust based on actual API)
        payload = {
            'event_id': event_id,
            'seats_count': seats_count,
            'payment_method': 'card'  # Adjust as needed
        }
        
        try:
            # Setup proxy for requests
            self._setup_proxy(self.session, self.proxy)
            
            # Make booking request (placeholder URL - replace with actual booking endpoint)
            response = self.session.post(
                'https://api.webook.com/api/v2/bookings',
                json=payload,
                headers=headers,
                timeout=CONFIG['api_timeout']
            )
            
            if response.status_code == 200:
                result = response.json()
                log(f"✓ Booking successful for {email}: {result}")
                return {'status': 'success', 'data': result}
            else:
                error_msg = f"Booking failed for {email}: {response.status_code}"
                try:
                    error_data = response.json()
                    error_msg += f" - {error_data.get('message', 'Unknown error')}"
                except:
                    pass
                log(error_msg)
                return {'status': 'failed', 'error': error_msg}
                
        except requests.exceptions.Timeout:
            error_msg = f"Booking timeout for {email}"
            log(error_msg)
            return {'status': 'timeout', 'error': error_msg}
        except Exception as e:
            error_msg = f"Booking error for {email}: {str(e)}"
            log(error_msg)
            return {'status': 'error', 'error': error_msg}
    
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

class BotGUI:
    """Main GUI application"""
    
    def __init__(self, root):
        self.root = root
        self.root.title("Webook Bot - Browser Automation")
        self.root.geometry("1200x700")
        
        # Data storage
        self.users = []
        self.proxies = []
        self.workers = []
        self.channels = []
        self.displayChannels = False
        self.stop_event = threading.Event()
        self.browser_semaphore = threading.Semaphore(1)  # Only 1 browser at a time

        
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
        ttk.Checkbutton(top_frame, text="Headless Browser", variable=self.headless_var).grid(row=0, column=6, padx=10)
        
        # Control buttons
        self.start_btn = ttk.Button(top_frame, text="Start Bot", command=self.start_bot)
        self.start_btn.grid(row=0, column=7, padx=5)
        
        self.stop_btn = ttk.Button(top_frame, text="Stop Bot", command=self.stop_bot, state='disabled')
        self.stop_btn.grid(row=0, column=8, padx=5)

        self.checkbox_frame = ttk.Frame(self.root)  # Create empty frame
        self.checkbox_frame.pack(fill='x', padx=10, pady=5)

        # Users table
        self._create_users_table()
        
        # Status bar
        self.status_var = tk.StringVar(value="Ready - Load users and proxies to begin")
        status_bar = ttk.Label(self.root, textvariable=self.status_var, relief='sunken')
        status_bar.pack(side='bottom', fill='x', padx=10, pady=5)
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
        
        self.displayChannels = True
    def _create_users_table(self):
        """Create the users display table"""
        table_frame = ttk.Frame(self.root)
        table_frame.pack(fill='both', expand=True, padx=10, pady=10)
        
        # Treeview for users
        columns = ('Email', 'Type', 'Proxy', 'Status', 'Seats Booked', 'Last Update')
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
                    'type': row.get('type', '').strip(),
                    'proxy': None,
                    'status': 'Ready',
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
            if seats_count < 1:
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
        for i, user in enumerate(self.users):
            worker = threading.Thread(
                target=self._worker_thread,
                args=(i, user, event_url, seats_count),
                daemon=True
            )
            self.workers.append(worker)
            worker.start()
        
        self.status_var.set(f"Bot started with {len(self.workers)} workers")
    
    def stop_bot(self):
        """Stop the bot operations"""
        self.stop_event.set()
        self.start_btn.config(state='normal')
        self.stop_btn.config(state='disabled')
        self.status_var.set("Stopping bot...")
    
    def _worker_thread(self, user_index, user, event_url, seats_count):
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
            # Wait for browser semaphore (queue for browser access)
            update_status("Waiting in queue...")
            log("Waiting for browser availability...")
    
            # BROWSER OPERATIONS - ONE AT A TIME
            with self.browser_semaphore:  # Only one browser at a time
                bot = WebookBot(
                    email=user['email'],
                    password=user['password'],
                    headless=self.headless_var.get(),
                    proxy=user.get('proxy')
                )
                
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
                    bot.extract_seatsio_chart_data()
                    path_parts = urlparse(event_url).path.strip("/").split("/")
                    event_id = path_parts[2]
                    bot.send_event_detail_request(event_id)
                    self.channels = bot.get_rendering_info()
                    if not self.displayChannels:
                        self._create_checkBox()
                    
                except Exception as e:
                    update_status("Login failed")
                    log(f"✗ Login failed: {str(e)}")
                    return
    
                # BOOKING OPERATIONS - ALL THREADS WORK SIMULTANEOUSLY (outside semaphore)
                '''if not self.stop_event.is_set():
                    update_status("Booking seats...")
                    log(f"Starting seat booking process ({seats_count} seats)")
                    
                    try:
                        result = bot.book_seat(
                            user['email'],
                            event_url,
                            seats_count,
                            log_callback=log
                        )
                        
                        if result['status'] == 'success':
                            user['seats_booked'] = seats_count
                            update_status("Booking successful")
                            log("✓ Seat booking completed successfully")
                            self.ui_queue.put(('update_user', user_index, 'seats_booked', seats_count))
                        else:
                            update_status("Booking failed")
                            log(f"✗ Seat booking failed: {result.get('error', 'Unknown error')}")
                            
                    except Exception as e:
                        update_status("Booking error")
                        log(f"✗ Booking error: {str(e)}")'''

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
                            if field in ['status', 'last_update']:
                                # Update the tree view
                                item_id = str(user_index)
                                if self.tree.exists(item_id):
                                    values = list(self.tree.item(item_id, 'values'))
                                    if field == 'status':
                                        values[3] = value
                                    elif field == 'last_update':
                                        values[5] = value
                                    self.tree.item(item_id, values=values)
                            elif field == 'seats_booked':
                                item_id = str(user_index)
                                if self.tree.exists(item_id):
                                    values = list(self.tree.item(item_id, 'values'))
                                    values[4] = str(value)
                                    self.tree.item(item_id, values=values)
                        
                    except queue.Empty:
                        break
                    except Exception as e:
                        print(f"UI update error: {e}")
                        
            finally:
                self.root.after(100, process_ui_queue)
        
        process_ui_queue()

def main():
    """Main application entry point"""
    root = tk.Tk()
    app = BotGUI(root)
    root.mainloop()

if __name__ == "__main__":
    main()
